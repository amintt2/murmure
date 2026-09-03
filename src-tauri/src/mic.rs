//! Autorisation micro (TCC) sur macOS. Ailleurs : toujours accordée.

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum MicStatus {
    Authorized,
    Denied,
    Undetermined,
    Restricted,
}

#[cfg(target_os = "macos")]
mod imp {
    use super::MicStatus;
    use objc2_av_foundation::{AVAuthorizationStatus, AVCaptureDevice, AVMediaTypeAudio};

    pub fn status() -> MicStatus {
        unsafe {
            let Some(media) = AVMediaTypeAudio else { return MicStatus::Authorized };
            let st = AVCaptureDevice::authorizationStatusForMediaType(media);
            match st {
                AVAuthorizationStatus::Authorized => MicStatus::Authorized,
                AVAuthorizationStatus::Denied => MicStatus::Denied,
                AVAuthorizationStatus::Restricted => MicStatus::Restricted,
                _ => MicStatus::Undetermined,
            }
        }
    }

    pub fn request() {
        let block = block2::RcBlock::new(|_granted: objc2::runtime::Bool| {});
        unsafe {
            if let Some(media) = AVMediaTypeAudio {
                AVCaptureDevice::requestAccessForMediaType_completionHandler(media, &block);
            }
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use super::MicStatus;
    pub fn status() -> MicStatus {
        MicStatus::Authorized
    }
    pub fn request() {}
}

pub fn status() -> MicStatus {
    imp::status()
}
pub fn request() {
    imp::request()
}
