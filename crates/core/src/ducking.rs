// Quieten everything else while dictating.
//
// Not in the brief; requested during dogfooding. Talking over music means the microphone
// picks the music up and the recogniser transcribes it, and it means you cannot hear the
// cues. Windows has an automatic ducking feature tied to the "communications" device
// role, but it only fires for streams opened with that role, it is a system-wide user
// setting we do not control, and its only levels are mute / -80% / -50% / nothing.
//
// So this does it explicitly: walk the audio sessions on the output device, remember each
// one's volume, scale it down, and put it back afterwards.

use anyhow::{anyhow, Result};
use windows::core::Interface;
use windows::Win32::Media::Audio::{
    eMultimedia, eRender, IAudioSessionControl2, IAudioSessionManager2, IMMDeviceEnumerator,
    ISimpleAudioVolume, MMDeviceEnumerator, AudioSessionStateExpired,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_ALL, COINIT_MULTITHREADED,
};

/// A session whose volume we changed, and what it was before.
struct Ducked {
    volume: ISimpleAudioVolume,
    original: f32,
}

/// Holds other applications quiet for as long as it is alive.
///
/// Restoring happens in `Drop`, so an error or a panic mid-dictation cannot leave the
/// user's music turned down with no way back short of the volume mixer.
pub struct Ducker {
    ducked: Vec<Ducked>,
    com_initialised: bool,
}

impl Ducker {
    /// Scales every other application's audio session to `level` (0.0 silent, 1.0
    /// unchanged). Returns a `Ducker` that restores them when dropped.
    pub fn start(level: f32) -> Result<Self> {
        let level = level.clamp(0.0, 1.0);
        let own_pid = std::process::id();

        // The worker thread does not otherwise use COM.
        let com_initialised = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).is_ok() };

        let result = unsafe { collect_sessions(own_pid, level) };
        match result {
            Ok(ducked) => Ok(Self {
                ducked,
                com_initialised,
            }),
            Err(e) => {
                if com_initialised {
                    unsafe { CoUninitialize() };
                }
                Err(e)
            }
        }
    }

    pub fn count(&self) -> usize {
        self.ducked.len()
    }
}

unsafe fn collect_sessions(own_pid: u32, level: f32) -> Result<Vec<Ducked>> {
    let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
        .map_err(|e| anyhow!("could not open the audio device enumerator: {e}"))?;

    // The multimedia render endpoint is where music and video play.
    let device = enumerator
        .GetDefaultAudioEndpoint(eRender, eMultimedia)
        .map_err(|e| anyhow!("no default output device: {e}"))?;

    let manager: IAudioSessionManager2 = device
        .Activate(CLSCTX_ALL, None)
        .map_err(|e| anyhow!("could not open the session manager: {e}"))?;

    let sessions = manager
        .GetSessionEnumerator()
        .map_err(|e| anyhow!("could not enumerate audio sessions: {e}"))?;

    let count = sessions.GetCount().unwrap_or(0);
    let mut ducked = Vec::new();

    for i in 0..count {
        let Ok(control) = sessions.GetSession(i) else {
            continue;
        };
        let Ok(control2) = control.cast::<IAudioSessionControl2>() else {
            continue;
        };

        // Never duck ourselves: the cues play through a session on this same device, and
        // turning them down is the opposite of what this is for.
        if control2.GetProcessId().map(|pid| pid == own_pid).unwrap_or(true) {
            continue;
        }

        // Expired sessions belong to processes that have gone away; anything else is
        // ducked, including sessions that are merely inactive right now. A paused video
        // that resumes mid-dictation would otherwise come back at full volume, which is
        // the exact moment this feature exists to prevent.
        if control.GetState().ok() == Some(AudioSessionStateExpired) {
            continue;
        }

        let Ok(volume) = control.cast::<ISimpleAudioVolume>() else {
            continue;
        };
        let Ok(original) = volume.GetMasterVolume() else {
            continue;
        };

        // Scale rather than set: an application already playing quietly should not get
        // louder because we ducked it.
        if volume.SetMasterVolume(original * level, std::ptr::null()).is_ok() {
            ducked.push(Ducked { volume, original });
        }
    }

    Ok(ducked)
}

impl Drop for Ducker {
    fn drop(&mut self) {
        for entry in &self.ducked {
            unsafe {
                let _ = entry.volume.SetMasterVolume(entry.original, std::ptr::null());
            }
        }
        if self.com_initialised {
            unsafe { CoUninitialize() };
        }
    }
}
