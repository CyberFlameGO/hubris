#[cfg(feature = "spi")]
use crate::common::{spi_read, spi_write};
use hif::*;
use ringbuf::*;
#[cfg(any(feature = "spi", feature = "gpio", feature = "i2c"))]
use userlib::*;

#[cfg(feature = "i2c")]
use drv_i2c_api::{Controller, I2cDevice, Mux, Port, ResponseCode, Segment};

#[cfg(feature = "i2c")]
declare_task!(I2C, i2c_driver);

#[cfg(feature = "gpio")]
declare_task!(GPIO, gpio_driver);

#[derive(Copy, Clone, PartialEq)]
enum Trace {
    Execute((usize, Op)),
    Failure(Failure),
    #[cfg(feature = "gpio")]
    GpioConfigure(
        drv_stm32h7_gpio_api::Port,
        u16,
        drv_stm32h7_gpio_api::Mode,
        drv_stm32h7_gpio_api::OutputType,
        drv_stm32h7_gpio_api::Speed,
        drv_stm32h7_gpio_api::Pull,
        drv_stm32h7_gpio_api::Alternate,
    ),
    #[cfg(feature = "gpio")]
    GpioInput(drv_stm32h7_gpio_api::Port),
    Success,
    None,
}

ringbuf!(Trace, 64, Trace::None);

pub struct Buffer(u8);
//
// The order in this enum must match the order in the functions array that
// is passed to execute.
//
pub enum Functions {
    #[cfg(feature = "i2c")]
    I2cRead(
        (Controller, Port, Mux, Segment, u8, u8, usize),
        ResponseCode,
    ),
    #[cfg(feature = "i2c")]
    I2cWrite(
        (Controller, Port, Mux, Segment, u8, u8, Buffer, usize),
        ResponseCode,
    ),
    #[cfg(feature = "gpio")]
    GpioInput(drv_stm32h7_gpio_api::Port, drv_stm32h7_gpio_api::GpioError),
    #[cfg(feature = "gpio")]
    GpioToggle(
        (drv_stm32h7_gpio_api::Port, u8),
        drv_stm32h7_gpio_api::GpioError,
    ),
    #[cfg(feature = "gpio")]
    GpioSet(
        (drv_stm32h7_gpio_api::Port, u8),
        drv_stm32h7_gpio_api::GpioError,
    ),
    #[cfg(feature = "gpio")]
    GpioReset(
        (drv_stm32h7_gpio_api::Port, u8),
        drv_stm32h7_gpio_api::GpioError,
    ),
    #[cfg(feature = "gpio")]
    GpioConfigure(
        (
            drv_stm32h7_gpio_api::Port,
            u8,
            drv_stm32h7_gpio_api::Mode,
            drv_stm32h7_gpio_api::OutputType,
            drv_stm32h7_gpio_api::Speed,
            drv_stm32h7_gpio_api::Pull,
            drv_stm32h7_gpio_api::Alternate,
        ),
        drv_stm32h7_gpio_api::GpioError,
    ),
    #[cfg(feature = "spi")]
    SpiRead((Task, usize, usize), drv_spi_api::SpiError),
    #[cfg(feature = "spi")]
    SpiWrite((Task, usize), drv_spi_api::SpiError),
}

#[cfg(feature = "i2c")]
fn i2c_args(
    stack: &[Option<u32>],
) -> Result<(Controller, Port, Option<(Mux, Segment)>, u8, Option<u8>), Failure>
{
    let controller = match stack[0] {
        Some(controller) => match Controller::from_u32(controller) {
            Some(controller) => controller,
            None => return Err(Failure::Fault(Fault::BadParameter(0))),
        },
        None => return Err(Failure::Fault(Fault::EmptyParameter(0))),
    };

    let port = match stack[1] {
        Some(port) => match Port::from_u32(port) {
            Some(port) => port,
            None => {
                return Err(Failure::Fault(Fault::BadParameter(1)));
            }
        },
        None => Port::Default,
    };

    let mux = match (stack[2], stack[3]) {
        (Some(mux), Some(segment)) => Some((
            match Mux::from_u32(mux) {
                Some(mux) => mux,
                None => {
                    return Err(Failure::Fault(Fault::BadParameter(2)));
                }
            },
            match Segment::from_u32(segment) {
                Some(segment) => segment,
                None => {
                    return Err(Failure::Fault(Fault::BadParameter(3)));
                }
            },
        )),
        _ => None,
    };

    let addr = match stack[4] {
        Some(addr) => addr as u8,
        None => return Err(Failure::Fault(Fault::EmptyParameter(4))),
    };

    let register = match stack[5] {
        Some(register) => Some(register as u8),
        None => None,
    };

    Ok((controller, port, mux, addr, register))
}

#[cfg(feature = "i2c")]
fn i2c_read(
    stack: &[Option<u32>],
    _data: &[u8],
    rval: &mut [u8],
) -> Result<usize, Failure> {
    if stack.len() < 7 {
        return Err(Failure::Fault(Fault::MissingParameters));
    }

    let fp = stack.len() - 7;
    let (controller, port, mux, addr, register) = i2c_args(&stack[fp..])?;

    let task = get_task_id(I2C);
    let device = I2cDevice::new(task, controller, port, mux, addr);

    match stack[fp + 6] {
        Some(nbytes) => {
            let n = nbytes as usize;

            if rval.len() < n {
                return Err(Failure::Fault(Fault::ReturnValueOverflow));
            }

            let res = if let Some(reg) = register {
                device.read_reg_into::<u8>(reg as u8, &mut rval[0..n])
            } else {
                device.read_into(&mut rval[0..n])
            };

            match res {
                Ok(rlen) => Ok(rlen),
                Err(err) => Err(Failure::FunctionError(err.into())),
            }
        }

        None => {
            if let Some(reg) = register {
                if rval.len() < 256 {
                    return Err(Failure::Fault(Fault::ReturnValueOverflow));
                }

                match device.read_block::<u8>(reg as u8, &mut rval[0..0xff]) {
                    Ok(rlen) => Ok(rlen),
                    Err(err) => Err(Failure::FunctionError(err.into())),
                }
            } else {
                Err(Failure::Fault(Fault::EmptyParameter(6)))
            }
        }
    }
}

#[cfg(feature = "i2c")]
fn i2c_write(
    stack: &[Option<u32>],
    _data: &[u8],
    _rval: &mut [u8],
) -> Result<usize, Failure> {
    let mut buf = [0u8; 17];

    //
    // We need at least 8 (!) parameters, the last of which is the number of
    // bytes to write.
    //
    if stack.len() < 8 {
        return Err(Failure::Fault(Fault::MissingParameters));
    }

    let len = match stack[stack.len() - 1] {
        Some(len) if len > 0 && len as usize <= buf.len() - 1 => {
            Ok(len as usize)
        }
        _ => Err(Failure::Fault(Fault::BadParameter(7))),
    }?;

    if stack.len() < 7 + len {
        return Err(Failure::Fault(Fault::MissingParameters));
    }

    let fp = stack.len() - (7 + len);
    let (controller, port, mux, addr, register) = i2c_args(&stack[fp..])?;

    let task = get_task_id(I2C);
    let device = I2cDevice::new(task, controller, port, mux, addr);

    let mut offs = 0;

    if let Some(register) = register {
        buf[offs] = register;
        offs += 1;
    }

    let bp = stack.len() - (1 + len);

    for i in 0..len {
        buf[i + offs] = match stack[bp + i] {
            None => {
                return Err(Failure::Fault(Fault::BadParameter(7)));
            }
            Some(val) => val as u8,
        }
    }

    match device.write(&buf[0..len + offs]) {
        Ok(_) => Ok(0),
        Err(err) => Err(Failure::FunctionError(err.into())),
    }
}

#[cfg(feature = "gpio")]
fn gpio_args(
    stack: &[Option<u32>],
) -> Result<(drv_stm32h7_gpio_api::Port, u16), Failure> {
    if stack.len() < 2 {
        return Err(Failure::Fault(Fault::MissingParameters));
    }

    let fp = stack.len() - 2;

    let port = match stack[fp + 0] {
        Some(port) => match drv_stm32h7_gpio_api::Port::from_u32(port) {
            Some(port) => port,
            None => return Err(Failure::Fault(Fault::BadParameter(0))),
        },
        None => return Err(Failure::Fault(Fault::EmptyParameter(0))),
    };

    let mask = match stack[fp + 1] {
        Some(pin) if pin < 16 => (1u16 << pin),
        Some(_) => {
            return Err(Failure::Fault(Fault::BadParameter(1)));
        }
        None => {
            return Err(Failure::Fault(Fault::EmptyParameter(1)));
        }
    };

    Ok((port, mask))
}

#[cfg(feature = "gpio")]
fn gpio_input(
    stack: &[Option<u32>],
    _data: &[u8],
    rval: &mut [u8],
) -> Result<usize, Failure> {
    use byteorder::ByteOrder;

    let task = get_task_id(GPIO);
    let gpio = drv_stm32h7_gpio_api::Gpio::from(task);

    if stack.len() < 1 {
        return Err(Failure::Fault(Fault::MissingParameters));
    }

    let fp = stack.len() - 1;

    let port = match stack[fp + 0] {
        Some(port) => match drv_stm32h7_gpio_api::Port::from_u32(port) {
            Some(port) => port,
            None => return Err(Failure::Fault(Fault::BadParameter(0))),
        },
        None => return Err(Failure::Fault(Fault::EmptyParameter(0))),
    };

    ringbuf_entry!(Trace::GpioInput(port));

    match gpio.read_input(port) {
        Ok(input) => {
            byteorder::LittleEndian::write_u16(rval, input);
            Ok(core::mem::size_of::<u16>())
        }
        Err(err) => Err(Failure::FunctionError(err.into())),
    }
}

#[cfg(feature = "gpio")]
fn gpio_toggle(
    stack: &[Option<u32>],
    _data: &[u8],
    _rval: &mut [u8],
) -> Result<usize, Failure> {
    let task = get_task_id(GPIO);
    let gpio = drv_stm32h7_gpio_api::Gpio::from(task);

    let (port, mask) = gpio_args(stack)?;

    match gpio.toggle(port, mask) {
        Ok(_) => Ok(0),
        Err(err) => Err(Failure::FunctionError(err.into())),
    }
}

#[cfg(feature = "gpio")]
fn gpio_set(
    stack: &[Option<u32>],
    _data: &[u8],
    _rval: &mut [u8],
) -> Result<usize, Failure> {
    let task = get_task_id(GPIO);
    let gpio = drv_stm32h7_gpio_api::Gpio::from(task);

    let (port, mask) = gpio_args(stack)?;

    match gpio.set_reset(port, mask, 0) {
        Ok(_) => Ok(0),
        Err(err) => Err(Failure::FunctionError(err.into())),
    }
}

#[cfg(feature = "gpio")]
fn gpio_reset(
    stack: &[Option<u32>],
    _data: &[u8],
    _rval: &mut [u8],
) -> Result<usize, Failure> {
    let task = get_task_id(GPIO);
    let gpio = drv_stm32h7_gpio_api::Gpio::from(task);

    let (port, mask) = gpio_args(stack)?;

    match gpio.set_reset(port, 0, mask) {
        Ok(_) => Ok(0),
        Err(err) => Err(Failure::FunctionError(err.into())),
    }
}

#[cfg(feature = "gpio")]
fn gpio_configure(
    stack: &[Option<u32>],
    _data: &[u8],
    _rval: &mut [u8],
) -> Result<usize, Failure> {
    use drv_stm32h7_gpio_api::*;

    if stack.len() < 7 {
        return Err(Failure::Fault(Fault::MissingParameters));
    }

    let fp = stack.len() - 7;
    let (port, mask) = gpio_args(&stack[fp..fp + 2])?;

    let mode = match stack[fp + 2] {
        Some(mode) => match Mode::from_u32(mode) {
            Some(mode) => mode,
            None => return Err(Failure::Fault(Fault::BadParameter(2))),
        },
        None => return Err(Failure::Fault(Fault::EmptyParameter(2))),
    };

    let output_type = match stack[fp + 3] {
        Some(output_type) => match OutputType::from_u32(output_type) {
            Some(output_type) => output_type,
            None => return Err(Failure::Fault(Fault::BadParameter(3))),
        },
        None => return Err(Failure::Fault(Fault::EmptyParameter(3))),
    };

    let speed = match stack[fp + 4] {
        Some(speed) => match Speed::from_u32(speed) {
            Some(speed) => speed,
            None => return Err(Failure::Fault(Fault::BadParameter(4))),
        },
        None => return Err(Failure::Fault(Fault::EmptyParameter(4))),
    };

    let pull = match stack[fp + 5] {
        Some(pull) => match Pull::from_u32(pull) {
            Some(pull) => pull,
            None => return Err(Failure::Fault(Fault::BadParameter(5))),
        },
        None => return Err(Failure::Fault(Fault::EmptyParameter(5))),
    };

    let af = match stack[fp + 6] {
        Some(af) => match Alternate::from_u32(af) {
            Some(af) => af,
            None => return Err(Failure::Fault(Fault::BadParameter(6))),
        },
        None => return Err(Failure::Fault(Fault::EmptyParameter(6))),
    };

    let task = get_task_id(GPIO);
    let gpio = drv_stm32h7_gpio_api::Gpio::from(task);

    #[rustfmt::skip]
    ringbuf_entry!(
        Trace::GpioConfigure(port, mask, mode, output_type, speed, pull, af)
    );

    match gpio.configure(port, mask, mode, output_type, speed, pull, af) {
        Ok(_) => Ok(0),
        Err(err) => Err(Failure::FunctionError(err.into())),
    }
}

pub(crate) static HIFFY_FUNCS: &[Function] = &[
    #[cfg(feature = "i2c")]
    i2c_read,
    #[cfg(feature = "i2c")]
    i2c_write,
    #[cfg(feature = "gpio")]
    gpio_input,
    #[cfg(feature = "gpio")]
    gpio_toggle,
    #[cfg(feature = "gpio")]
    gpio_set,
    #[cfg(feature = "gpio")]
    gpio_reset,
    #[cfg(feature = "gpio")]
    gpio_configure,
    #[cfg(feature = "spi")]
    spi_read,
    #[cfg(feature = "spi")]
    spi_write,
];

//
// This definition forces the compiler to emit the DWARF needed for debuggers
// to be able to know function indices, arguments and return values.
//
#[no_mangle]
static HIFFY_FUNCTIONS: Option<&Functions> = None;

pub(crate) fn trace_execute(offset: usize, op: hif::Op) {
    ringbuf_entry!(Trace::Execute((offset, op)));
}

pub(crate) fn trace_success() {
    ringbuf_entry!(Trace::Success);
}

pub(crate) fn trace_failure(f: hif::Failure) {
    ringbuf_entry!(Trace::Failure(f));
}