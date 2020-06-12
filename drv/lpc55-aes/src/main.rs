//! A driver for the LPC55 AES block
//!
//! Currently just supports AES-ECB. Should certainly support more later.
//!
//! This hardware block really wants to be interrupt driven and seems to
//! stall if you try and use less than two blocks. It's also easy to get
//! out of sequence if you forget to read the output data. Some of this
//! oddness may be related to timing. It seems like some of the engines
//! may have timing requirements we need to wait for.
//!
//! There also seems to be something odd with the endianness (see also
//! setting numerous bits to endian swap and the call in getting the
//! data) which may just be related to the choice of data...
//!
//! # IPC protocol
//!
//! ## `encrypt` (1)
//!
//! Encrypts the contents of lease #0 (R) to lease #1 (RW) using key (arg #0)
//! Only supports AES-ECB mode

#![no_std]
#![no_main]

use lpc55_pac as device;
use zerocopy::AsBytes;
use userlib::*;

#[cfg(not(feature = "standalone"))]
const SYSCON: Task = Task::syscon_driver;

// For standalone mode -- this won't work, but then, neither will a task without
// a kernel.
#[cfg(feature = "standalone")]
const SYSCON: Task = SELF;

#[derive(Copy, Clone, Debug, FromPrimitive)]
enum Operation {
    Encrypt = 1
}

#[repr(u32)]
enum ResponseCode {
    BadArg = 2,
    Busy = 3,
}

impl From<ResponseCode> for u32 {
    fn from(rc: ResponseCode) -> Self {
        rc as u32
    }
}

struct CryptData {
    caller: hl::Caller<()>,
    len: usize,
    rpos: usize,
    wpos: usize,
}

#[export_name = "main"]
fn main() -> ! {
    // Turn the actual peripheral on so that we can interact with it.
    turn_on_aes();

    let aes = unsafe { &*device::HASHCRYPT::ptr() };

    // Always use AES
    // TODO other modes besides ECB (probably just pass IV as another lease)
    aes.ctrl.modify(|_, w| w.mode().aes()
                            .hashswpb().set_bit()
                            );

    aes.cryptcfg.modify(|_, w|
            w.aesmode().ecb()
            .aesdecrypt().encrypt()
            .aessecret().normal_way()
            .aeskeysz().bits_128()
            .msw1st_out().set_bit()
            .msw1st().set_bit()
            .swapkey().set_bit()
            .swapdat().set_bit()
            );

    sys_irq_control(1, true);

    // Field messages.
    let mask = 1;
    let mut c: Option<CryptData> = None;

    let mut buffer = [0; 16];
    loop {
        //let msginfo = sys_recv(key.as_bytes_mut(), mask);
        hl::recv(
            &mut buffer,
            mask,
            &mut c,
            |cryptref, bits| {

                if bits & 1 != 0 {
                    // We alternate between setting the waiting and the digest
                    // interrupt depending on where we're processing
                    // Because of how this is structured, we purposely do
                    // the read first in the block to ensure we don't overwrite
                    // the data
                    //
                    // TODO Make this less prone to sequencing problems by
                    // doing a load of data and a write on the same interrupt
                    if aes.status.read().digest().bit() {
                        get_data(&aes, cryptref)
                    } else if aes.status.read().waiting().bit() {
                        // Shove more data to the block
                        load_a_block(&aes, cryptref)
                    } else if aes.status.read().error().bit() {
                        cortex_m_semihosting::hprintln!("AES error").ok();
                    }
                    sys_irq_control(1, true);
                }
            },
            |cryptref, op, msg| match op {
                Operation::Encrypt => {
                    let (&key, caller) = msg.fixed_with_leases::<[u32; 4], ()>(2)
                                        .ok_or(ResponseCode::BadArg)?;

                    if cryptref.is_some() {
                        return Err(ResponseCode::Busy);
                    }

                    let src = caller.borrow(0);
                    let src_info = src.info().ok_or(ResponseCode::BadArg)?;

                    if !src_info.attributes.contains(LeaseAttributes::READ) {
                        return Err(ResponseCode::BadArg);
                    }

                    let dst = caller.borrow(1);
                    let dst_info = dst.info().ok_or(ResponseCode::BadArg)?;

                    if !dst_info.attributes.contains(LeaseAttributes::WRITE) {
                        return Err(ResponseCode::BadArg);
                    }

                    if src_info.len != dst_info.len {
                        return Err(ResponseCode::BadArg);
                    }

                    // Kick off a new hash
                    aes.ctrl.modify(|_, w| w.new_hash().start());

                    // This isn't well specified in the documentation but
                    // there's occasionally issues with the NEEDKEY bit never
                    // clearing, acting as if the writes to INDATA didn't go
                    // through. The documentation says NEW_HASH should be
                    // self-clearing after one clock cycle so there are
                    // two plausible theories for why the barriers work:
                    // the write to NEW_HASH needs to complete
                    // before we write the key or we need to ensure the engine
                    // has actually had enough time to startup because it takes
                    // more than one clock cycle. The way the docs are written
                    // it sounds like we should poll on this field but that's
                    // not accessible through the crate.
                    cortex_m::asm::dmb();
                    cortex_m::asm::isb();

                    // This is our key. The hardware only supports 128-bit,
                    // 192-bit and 256-bit keys
                    //
                    // TODO go back and support the other key sizes

                    unsafe {
                        aes.indata.write( |w| w.data().bits(key[0]) );
                        aes.indata.write( |w| w.data().bits(key[1]) );
                        aes.indata.write( |w| w.data().bits(key[2]) );
                        aes.indata.write( |w| w.data().bits(key[3]) );
                    }

                    // wait for the key to be loaded. We could potentially
                    // loop forever if we haven't set up the key correctly
                    // but looping forever is actually better behavior than
                    // potentially interrupting forever with the AES block.
                    //
                    // NEEDKEY also sets the waiting interrupt so maybe
                    // it would be cleaner just to do that on the first
                    // interrupt?
                    while aes.status.read().needkey().bit() { }

                    *cryptref = Some(CryptData {
                        caller,
                        rpos: 0,
                        wpos: 0,
                        len: dst_info.len,
                    });

                    aes.intenset.modify(|_, w| w.waiting().set_bit());
                    Ok(())
                },
            }

        );
    }
}


fn turn_on_aes() {
    let rcc_driver = TaskId::for_index_and_gen(SYSCON as usize, Generation::default());

    const ENABLE_CLOCK: u16 = 1;
    let pnum = 82; // see bits in APB1ENR
    let (code, _) = userlib::sys_send(rcc_driver, ENABLE_CLOCK, pnum.as_bytes(), &mut [], &[]);
    assert_eq!(code, 0);

    const LEAVE_RESET: u16 = 4;
    let (code, _) = userlib::sys_send(rcc_driver, LEAVE_RESET, pnum.as_bytes(), &mut [], &[]);
    assert_eq!(code, 0);
}

fn load_a_block(aes: &device::hashcrypt::RegisterBlock, c: &mut Option<CryptData>) {
    let cdata = if let Some(cdata) = c {
            cdata
        } else {
            return
        };

    if let Some(data) = cdata.caller.borrow(0).read_at::<[u32; 4]>(cdata.rpos) {
        unsafe {
            aes.indata.write( |w| w.data().bits(data[0]) );
            aes.indata.write( |w| w.data().bits(data[1]) );
            aes.indata.write( |w| w.data().bits(data[2]) );
            aes.indata.write( |w| w.data().bits(data[3]) );
            cdata.rpos += 16
        }

        aes.intenset.modify(|_, w| w.digest().set_bit());
    } else {
        core::mem::replace(c, None).unwrap().caller.reply_fail(ResponseCode::BadArg);
    }
}


fn get_data(aes: &device::hashcrypt::RegisterBlock, c: &mut Option<CryptData>) {
    let mut data : [u32; 4] = [0; 4];

    let cdata = if let Some(cdata) = c {
            cdata
        } else {
            return
        };

    data[0] = u32::from_be(aes.digest0[0].read().digest().bits());
    data[1] = u32::from_be(aes.digest0[1].read().digest().bits());
    data[2] = u32::from_be(aes.digest0[2].read().digest().bits());
    data[3] = u32::from_be(aes.digest0[3].read().digest().bits());

    if let Some(()) = cdata.caller.borrow(1).write_at::<[u32; 4]>(cdata.wpos, data) {
        cdata.wpos += 16;
        if cdata.wpos == cdata.len {
            aes.intenclr.write(|w| w.digest().set_bit().
                                    waiting().set_bit());

            core::mem::replace(c, None).unwrap().caller.reply(());
        }
    } else {
        core::mem::replace(c, None).unwrap().caller.reply_fail(ResponseCode::BadArg);
    }
}