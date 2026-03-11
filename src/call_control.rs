use crossbeam_channel::{Receiver, Sender};
use std::sync::Arc;

pub type CallError = Box<dyn std::error::Error + Send + Sync>;

/// Trait abstracting xphone::Call methods used by REST API handlers.
/// Enables unit testing without a real SIP connection.
pub trait CallControl: Send + Sync {
    fn hold(&self) -> Result<(), CallError>;
    fn resume(&self) -> Result<(), CallError>;
    fn mute(&self) -> Result<(), CallError>;
    fn unmute(&self) -> Result<(), CallError>;
    fn blind_transfer(&self, target: &str) -> Result<(), CallError>;
    fn send_dtmf(&self, digit: &str) -> Result<(), CallError>;
    fn end(&self) -> Result<(), CallError>;
    fn pcm_writer(&self) -> Option<Sender<Vec<i16>>>;
    fn paced_pcm_writer(&self) -> Option<Sender<Vec<i16>>>;
    fn pcm_reader(&self) -> Option<Receiver<Vec<i16>>>;
}

/// Wrapper around `Arc<xphone::Call>` implementing `CallControl`.
pub struct XphoneCall(pub(crate) Arc<xphone::Call>);

impl CallControl for XphoneCall {
    fn hold(&self) -> Result<(), CallError> {
        self.0.hold().map_err(|e| Box::new(e) as CallError)
    }
    fn resume(&self) -> Result<(), CallError> {
        self.0.resume().map_err(|e| Box::new(e) as CallError)
    }
    fn mute(&self) -> Result<(), CallError> {
        self.0.mute().map_err(|e| Box::new(e) as CallError)
    }
    fn unmute(&self) -> Result<(), CallError> {
        self.0.unmute().map_err(|e| Box::new(e) as CallError)
    }
    fn blind_transfer(&self, target: &str) -> Result<(), CallError> {
        self.0
            .blind_transfer(target)
            .map_err(|e| Box::new(e) as CallError)
    }
    fn send_dtmf(&self, digit: &str) -> Result<(), CallError> {
        self.0
            .send_dtmf(digit)
            .map_err(|e| Box::new(e) as CallError)
    }
    fn end(&self) -> Result<(), CallError> {
        self.0.end().map_err(|e| Box::new(e) as CallError)
    }
    fn pcm_writer(&self) -> Option<Sender<Vec<i16>>> {
        self.0.pcm_writer()
    }
    fn paced_pcm_writer(&self) -> Option<Sender<Vec<i16>>> {
        self.0.paced_pcm_writer()
    }
    fn pcm_reader(&self) -> Option<Receiver<Vec<i16>>> {
        self.0.pcm_reader()
    }
}

#[cfg(test)]
pub(crate) mod mock {
    use super::*;
    use std::sync::Mutex;

    /// Mock call for unit tests. All operations succeed by default.
    pub struct MockCall {
        pub hold_ok: bool,
        pub resume_ok: bool,
        pub mute_ok: bool,
        pub unmute_ok: bool,
        pub transfer_ok: bool,
        pub dtmf_ok: bool,
        pub end_ok: bool,
        pub pcm_tx: Option<Sender<Vec<i16>>>,
        pub pcm_rx: Option<Receiver<Vec<i16>>>,
        /// Records digits sent via send_dtmf.
        pub dtmf_log: Mutex<Vec<String>>,
        /// Records target passed to blind_transfer.
        pub transfer_log: Mutex<Vec<String>>,
    }

    impl Default for MockCall {
        fn default() -> Self {
            Self {
                hold_ok: true,
                resume_ok: true,
                mute_ok: true,
                unmute_ok: true,
                transfer_ok: true,
                dtmf_ok: true,
                end_ok: true,
                pcm_tx: None,
                pcm_rx: None,
                dtmf_log: Mutex::new(Vec::new()),
                transfer_log: Mutex::new(Vec::new()),
            }
        }
    }

    impl MockCall {
        pub fn with_pcm_channels() -> (Self, Receiver<Vec<i16>>, Sender<Vec<i16>>) {
            let (play_tx, play_rx) = crossbeam_channel::unbounded();
            let (audio_tx, audio_rx) = crossbeam_channel::unbounded();
            let mock = Self {
                pcm_tx: Some(play_tx),
                pcm_rx: Some(audio_rx),
                ..Default::default()
            };
            (mock, play_rx, audio_tx)
        }
    }

    fn mock_err(msg: &str) -> CallError {
        msg.into()
    }

    impl CallControl for MockCall {
        fn hold(&self) -> Result<(), CallError> {
            if self.hold_ok {
                Ok(())
            } else {
                Err(mock_err("hold failed"))
            }
        }
        fn resume(&self) -> Result<(), CallError> {
            if self.resume_ok {
                Ok(())
            } else {
                Err(mock_err("resume failed"))
            }
        }
        fn mute(&self) -> Result<(), CallError> {
            if self.mute_ok {
                Ok(())
            } else {
                Err(mock_err("mute failed"))
            }
        }
        fn unmute(&self) -> Result<(), CallError> {
            if self.unmute_ok {
                Ok(())
            } else {
                Err(mock_err("unmute failed"))
            }
        }
        fn blind_transfer(&self, target: &str) -> Result<(), CallError> {
            if self.transfer_ok {
                self.transfer_log.lock().unwrap().push(target.to_string());
                Ok(())
            } else {
                Err(mock_err("transfer failed"))
            }
        }
        fn send_dtmf(&self, digit: &str) -> Result<(), CallError> {
            if self.dtmf_ok {
                self.dtmf_log.lock().unwrap().push(digit.to_string());
                Ok(())
            } else {
                Err(mock_err("dtmf failed"))
            }
        }
        fn end(&self) -> Result<(), CallError> {
            if self.end_ok {
                Ok(())
            } else {
                Err(mock_err("end failed"))
            }
        }
        fn pcm_writer(&self) -> Option<Sender<Vec<i16>>> {
            self.pcm_tx.clone()
        }
        fn paced_pcm_writer(&self) -> Option<Sender<Vec<i16>>> {
            self.pcm_tx.clone()
        }
        fn pcm_reader(&self) -> Option<Receiver<Vec<i16>>> {
            self.pcm_rx.clone()
        }
    }
}
