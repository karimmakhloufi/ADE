//! "Now playing": reads the Windows media sessions (GSMTC). Browsers playing
//! Spotify (or any other media) publish the title and artist there, and
//! accept play/pause/next/previous commands.

#[derive(Debug, Clone, PartialEq, Default)]
pub struct NowPlaying {
    pub title: String,
    pub artist: String,
    pub playing: bool,
}

#[derive(Debug, Clone, Copy)]
pub enum Command {
    PlayPause,
    Next,
    Previous,
}

/// The most relevant media session (playing first, Spotify first), if any.
pub async fn current() -> Option<NowPlaying> {
    blocking(imp::current).await.flatten()
}

/// Sends a command, then returns the updated state.
pub async fn control(cmd: Command) -> Option<NowPlaying> {
    blocking(move || {
        imp::control(cmd);
        // Let the player apply the command before reading the new state.
        std::thread::sleep(std::time::Duration::from_millis(300));
        imp::current()
    })
    .await
    .flatten()
}

/// The WinRT calls block: run them off the UI executor.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    let (tx, rx) = futures::channel::oneshot::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.await.ok()
}

#[cfg(windows)]
mod imp {
    use super::{Command, NowPlaying};
    use windows::core::Result;
    use windows::Media::Control::{
        GlobalSystemMediaTransportControlsSession as Session,
        GlobalSystemMediaTransportControlsSessionManager as Manager,
        GlobalSystemMediaTransportControlsSessionPlaybackStatus as Status,
    };

    fn is_playing(s: &Session) -> bool {
        s.GetPlaybackInfo()
            .and_then(|i| i.PlaybackStatus())
            .is_ok_and(|st| st == Status::Playing)
    }

    fn best_session() -> Result<Option<Session>> {
        let mgr = Manager::RequestAsync()?.get()?;
        let sessions = mgr.GetSessions()?;
        let mut best: Option<(u8, Session)> = None;
        for i in 0..sessions.Size()? {
            let s = sessions.GetAt(i)?;
            let app = s.SourceAppUserModelId().map(|h| h.to_string()).unwrap_or_default();
            let mut score = 0;
            if is_playing(&s) {
                score += 2;
            }
            if app.to_lowercase().contains("spotify") {
                score += 1;
            }
            if best.as_ref().map_or(true, |(b, _)| score > *b) {
                best = Some((score, s));
            }
        }
        Ok(best.map(|(_, s)| s).or_else(|| mgr.GetCurrentSession().ok()))
    }

    fn read(s: &Session) -> Result<NowPlaying> {
        let props = s.TryGetMediaPropertiesAsync()?.get()?;
        Ok(NowPlaying {
            title: props.Title()?.to_string(),
            artist: props.Artist()?.to_string(),
            playing: is_playing(s),
        })
    }

    pub fn current() -> Option<NowPlaying> {
        let s = best_session().ok()??;
        read(&s).ok().filter(|np| !np.title.trim().is_empty())
    }

    pub fn control(cmd: Command) {
        let Ok(Some(s)) = best_session() else { return };
        let op = match cmd {
            Command::PlayPause => s.TryTogglePlayPauseAsync(),
            Command::Next => s.TrySkipNextAsync(),
            Command::Previous => s.TrySkipPreviousAsync(),
        };
        if let Ok(op) = op {
            let _ = op.get();
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use super::{Command, NowPlaying};

    pub fn current() -> Option<NowPlaying> {
        None
    }

    pub fn control(_cmd: Command) {}
}
