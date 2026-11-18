use thiserror::Error;

#[derive(Debug, Error)]
pub enum RazerError {
    #[error("HID error: {0}")]
    Hid(#[from] hidapi::HidError),

    #[error("Device not found: VID={vid:#06x} PID={pid:#06x}")]
    DeviceNotFound { vid: u16, pid: u16 },

    #[error("Invalid response from device (status={status:#04x})")]
    InvalidResponse { status: u8 },

    #[error("Command timed out after {attempts} attempts")]
    Timeout { attempts: u8 },

    #[error("CRC mismatch (expected {expected:#04x}, got {got:#04x})")]
    CrcMismatch { expected: u8, got: u8 },

    #[error("No response data from device")]
    NoData,
}

pub type Result<T> = std::result::Result<T, RazerError>;
