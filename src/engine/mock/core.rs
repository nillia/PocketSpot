//! The mock engine's state machine. Pure: time is always passed in as
//! `now_ms` (Unix milliseconds), so tests drive it through hours of
//! playback without sleeping.

use super::catalog;
use crate::{
    engine::Refusal,
    protocol::{
        ActiveDevice, Command, FailureKind, PlayState, Playback, PlaybackContext, Reject, Session,
        Snapshot,
    },
};
use std::time::Duration;

const PAIR_AFTER_MS: u64 = 700;
const CONNECT_MS: u64 = 600;
const RECONNECT_MS: u64 = 2_000;
const PAIRING_LIFETIME_MS: u64 = 10 * 60 * 1000;
/// "Previous" restarts the track instead when it has played longer than this.
const RESTART_THRESHOLD_MS: u32 = 3_000;
pub(super) const PAIRING_URL: &str = "https://www.spotify.com/pair";
pub(super) const PAIRING_CODE: &str = "DEMO-1234";

#[derive(Clone, Copy, Debug)]
pub struct MockOptions {
    /// Start signed in, skipping pairing.
    pub signed_in: bool,
    /// Pretend the user approves the pairing code after this long; `None`
    /// waits forever.
    pub approve_after: Option<Duration>,
    /// Seed for shuffle, so runs are reproducible.
    pub seed: u64,
}

impl Default for MockOptions {
    fn default() -> Self {
        Self {
            signed_in: false,
            approve_after: Some(Duration::from_secs(8)),
            seed: 0x5eed,
        }
    }
}

/// Failures the real engine can run into, replayed with their timeline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    /// The connection to Spotify drops: `Failed { Network }` with a retry
    /// time, then `Connecting`, `Ready`, and playback resumes if it was
    /// playing.
    NetworkLoss,
    /// Spotify rejects the saved login: back to pairing, nothing playing.
    LoginRejected,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Timer {
    ShowPairing,
    Approve,
    Connected,
    Reconnect,
}

/// What to restore after a reconnect.
#[derive(Clone, Debug)]
struct Resume {
    context_uri: String,
    index: usize,
    position_ms: u32,
}

pub struct MockCore {
    options: MockOptions,
    state: Snapshot,
    context_uri: Option<String>,
    queue: Vec<usize>,
    index: usize,
    timers: Vec<(u64, Timer)>,
    resume: Option<Resume>,
    seed: u64,
}

impl MockCore {
    pub fn new(options: MockOptions, now_ms: u64) -> Self {
        let mut core = Self {
            options,
            state: Snapshot {
                playback: Playback {
                    volume: 50,
                    ..Playback::default()
                },
                ..Snapshot::default()
            },
            context_uri: None,
            queue: Vec::new(),
            index: 0,
            timers: Vec::new(),
            resume: None,
            seed: options.seed | 1,
        };
        if options.signed_in {
            core.state.session = Session::Ready;
        } else {
            core.schedule(now_ms + PAIR_AFTER_MS, Timer::ShowPairing);
        }
        core
    }

    /// The state to publish (its revision is managed by the publisher).
    pub fn state(&self) -> &Snapshot {
        &self.state
    }

    /// When [`advance`](Self::advance) next has something to do.
    pub fn next_deadline(&self) -> Option<u64> {
        let timer = self.timers.iter().map(|(at, _)| *at).min();
        timer.into_iter().chain(self.track_end()).min()
    }

    /// Carry out everything due by `now_ms`: timers and track ends.
    pub fn advance(&mut self, now_ms: u64) {
        while let Some(at) = self.next_deadline().filter(|at| *at <= now_ms) {
            if let Some(position) = self.timers.iter().position(|(due, _)| *due == at) {
                let (_, timer) = self.timers.remove(position);
                self.fire(timer, at);
            } else {
                // The current track ended at `at`.
                self.skip_forward(at);
            }
        }
    }

    pub fn command(&mut self, command: Command, now_ms: u64) -> Result<(), Refusal> {
        self.advance(now_ms);
        if self.state.session != Session::Ready {
            return Err(Refusal::new(Reject::NotReady, "not signed in yet"));
        }
        match command {
            Command::Play {
                context_uri,
                track_uri,
            } => self.play(&context_uri, track_uri.as_deref(), now_ms),
            Command::Pause => self.pause(now_ms),
            Command::Resume => self.resume_playing(now_ms),
            Command::Next => {
                self.require_queue()?;
                self.skip_forward(now_ms);
                Ok(())
            }
            Command::Previous => self.previous(now_ms),
            Command::SetVolume { percent } if percent <= 100 => {
                self.state.playback.volume = percent;
                Ok(())
            }
            Command::SetVolume { .. } => Err(Refusal::new(
                Reject::Invalid,
                "volume must be between 0 and 100",
            )),
            Command::SetShuffle { enabled } => {
                self.state.playback.shuffle = enabled;
                Ok(())
            }
            Command::Stop => {
                self.stop();
                Ok(())
            }
        }
    }

    /// Forget the account and return to pairing.
    pub fn logout(&mut self, now_ms: u64) {
        self.advance(now_ms);
        self.forget_account();
        self.show_pairing(now_ms);
    }

    pub fn inject(&mut self, fault: Fault, now_ms: u64) {
        self.advance(now_ms);
        if self.state.session != Session::Ready {
            return;
        }
        match fault {
            Fault::NetworkLoss => {
                self.resume = self.playing_now(now_ms);
                self.stop();
                self.state.session = Session::Failed {
                    kind: FailureKind::Network,
                    message: "Connection to Spotify lost".into(),
                    retry_at_ms: Some(now_ms + RECONNECT_MS),
                };
                self.schedule(now_ms + RECONNECT_MS, Timer::Reconnect);
            }
            Fault::LoginRejected => {
                self.forget_account();
                self.show_pairing(now_ms);
            }
        }
    }

    fn fire(&mut self, timer: Timer, at: u64) {
        match timer {
            Timer::ShowPairing => self.show_pairing(at),
            Timer::Approve | Timer::Reconnect => {
                self.state.session = Session::Connecting;
                self.schedule(at + CONNECT_MS, Timer::Connected);
            }
            Timer::Connected => {
                self.state.session = Session::Ready;
                if let Some(resume) = self.resume.take() {
                    self.start(&resume.context_uri, resume.index, resume.position_ms, at);
                }
            }
        }
    }

    fn schedule(&mut self, at: u64, timer: Timer) {
        self.timers.push((at, timer));
    }

    fn show_pairing(&mut self, now_ms: u64) {
        self.timers.clear();
        self.state.session = Session::Pairing {
            url: PAIRING_URL.into(),
            code: PAIRING_CODE.into(),
            expires_at_ms: now_ms + PAIRING_LIFETIME_MS,
        };
        if let Some(after) = self.options.approve_after {
            let after = u64::try_from(after.as_millis()).unwrap_or(u64::MAX);
            self.schedule(now_ms.saturating_add(after), Timer::Approve);
        }
    }

    fn forget_account(&mut self) {
        self.stop();
        self.resume = None;
        self.context_uri = None;
        self.queue.clear();
        self.state.playback.track = None;
        self.state.playback.context = None;
    }

    fn play(
        &mut self,
        context_uri: &str,
        track_uri: Option<&str>,
        now_ms: u64,
    ) -> Result<(), Refusal> {
        let Some((_, tracks)) = catalog::context(context_uri) else {
            return Err(Refusal::new(Reject::Invalid, "unknown playlist or album"));
        };
        let index = match track_uri {
            None => 0,
            Some(uri) => tracks
                .iter()
                .position(|&n| catalog::track(n).uri == uri)
                .ok_or_else(|| {
                    Refusal::new(Reject::Invalid, "that track is not in this context")
                })?,
        };
        self.start(context_uri, index, 0, now_ms);
        Ok(())
    }

    /// Load track `index` of `context_uri` at `position_ms` and play it.
    fn start(&mut self, context_uri: &str, index: usize, position_ms: u32, now_ms: u64) {
        let Some((name, tracks)) = catalog::context(context_uri) else {
            return;
        };
        self.queue = tracks;
        self.index = index.min(self.queue.len().saturating_sub(1));
        self.context_uri = Some(context_uri.to_owned());
        let playback = &mut self.state.playback;
        playback.context = Some(PlaybackContext {
            uri: context_uri.to_owned(),
            name: Some(name.to_owned()),
        });
        playback.track = Some(catalog::track(self.queue[self.index]));
        playback.state = PlayState::Playing;
        playback.position_ms = position_ms;
        playback.position_at_ms = now_ms;
        self.state.device = ActiveDevice::Local;
    }

    fn require_queue(&self) -> Result<(), Refusal> {
        if self.queue.is_empty() {
            Err(Refusal::new(Reject::Invalid, "nothing is playing"))
        } else {
            Ok(())
        }
    }

    fn pause(&mut self, now_ms: u64) -> Result<(), Refusal> {
        match self.state.playback.state {
            PlayState::Playing | PlayState::Buffering => {
                self.sample_position(now_ms);
                self.state.playback.state = PlayState::Paused;
                Ok(())
            }
            PlayState::Paused => Ok(()),
            PlayState::Stopped => Err(Refusal::new(Reject::Invalid, "nothing is playing")),
        }
    }

    fn resume_playing(&mut self, now_ms: u64) -> Result<(), Refusal> {
        match self.state.playback.state {
            PlayState::Paused => {
                self.state.playback.position_at_ms = now_ms;
                self.state.playback.state = PlayState::Playing;
                self.state.device = ActiveDevice::Local;
                Ok(())
            }
            PlayState::Playing | PlayState::Buffering => Ok(()),
            PlayState::Stopped => Err(Refusal::new(Reject::Invalid, "nothing is playing")),
        }
    }

    fn previous(&mut self, now_ms: u64) -> Result<(), Refusal> {
        self.require_queue()?;
        let context = self.context_uri.clone().unwrap_or_default();
        let played = self.state.playback.position_at(now_ms);
        let index = if played > RESTART_THRESHOLD_MS {
            self.index
        } else {
            self.index.saturating_sub(1)
        };
        self.start(&context, index, 0, now_ms);
        Ok(())
    }

    /// Move to the next track (a different random one with shuffle), or
    /// stop after the last one, as Spotify does without repeat.
    fn skip_forward(&mut self, now_ms: u64) {
        let len = self.queue.len();
        let next = if self.state.playback.shuffle && len > 1 {
            self.seed ^= self.seed << 13;
            self.seed ^= self.seed >> 7;
            self.seed ^= self.seed << 17;
            let offset = usize::try_from(self.seed % (len as u64 - 1)).unwrap_or(0);
            Some((self.index + 1 + offset) % len)
        } else {
            Some(self.index + 1).filter(|next| *next < len)
        };
        match (next, self.context_uri.clone()) {
            (Some(index), Some(context)) => self.start(&context, index, 0, now_ms),
            _ => self.stop(),
        }
    }

    fn stop(&mut self) {
        let playback = &mut self.state.playback;
        playback.state = PlayState::Stopped;
        playback.position_ms = 0;
        self.state.device = ActiveDevice::None;
    }

    fn sample_position(&mut self, now_ms: u64) {
        let playback = &mut self.state.playback;
        playback.position_ms = playback.position_at(now_ms);
        playback.position_at_ms = now_ms;
    }

    /// When the current track ends, if it is playing.
    fn track_end(&self) -> Option<u64> {
        let playback = &self.state.playback;
        let track = playback.track.as_ref()?;
        (playback.state == PlayState::Playing).then(|| {
            playback.position_at_ms
                + u64::from(track.duration_ms.saturating_sub(playback.position_ms))
        })
    }

    /// What plays now, if anything, as something to restore later.
    fn playing_now(&self, now_ms: u64) -> Option<Resume> {
        (self.state.playback.state == PlayState::Playing).then(|| Resume {
            context_uri: self.context_uri.clone().unwrap_or_default(),
            index: self.index,
            position_ms: self.state.playback.position_at(now_ms),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T0: u64 = 1_000_000;
    const PLAYLIST: &str = "spotify:playlist:PocketSpotDemoList0001";

    fn signed_in() -> MockCore {
        MockCore::new(
            MockOptions {
                signed_in: true,
                ..MockOptions::default()
            },
            T0,
        )
    }

    fn title(core: &MockCore) -> &str {
        &core.state().playback.track.as_ref().unwrap().title
    }

    fn play(core: &mut MockCore) {
        let command = Command::Play {
            context_uri: PLAYLIST.into(),
            track_uri: None,
        };
        core.command(command, T0).unwrap();
    }

    #[test]
    fn pairs_connects_and_becomes_ready_on_its_timeline() {
        let mut core = MockCore::new(MockOptions::default(), T0);
        assert_eq!(core.state().session, Session::Starting);
        core.advance(T0 + PAIR_AFTER_MS);
        assert!(matches!(
            &core.state().session,
            Session::Pairing { code, .. } if code == PAIRING_CODE
        ));
        core.advance(T0 + PAIR_AFTER_MS + 8_000);
        assert_eq!(core.state().session, Session::Connecting);
        core.advance(T0 + PAIR_AFTER_MS + 8_000 + CONNECT_MS);
        assert_eq!(core.state().session, Session::Ready);
        assert_eq!(core.next_deadline(), None);
    }

    #[test]
    fn commands_need_a_ready_session() {
        let mut core = MockCore::new(MockOptions::default(), T0);
        let refusal = core.command(Command::Next, T0).unwrap_err();
        assert_eq!(refusal.reason, Reject::NotReady);
    }

    #[test]
    fn plays_a_playlist_from_the_start_or_from_a_track() {
        let mut core = signed_in();
        play(&mut core);
        let state = core.state();
        assert_eq!(state.playback.state, PlayState::Playing);
        assert_eq!(state.device, ActiveDevice::Local);
        assert_eq!(title(&core), "Night Signals");
        assert_eq!(
            core.state()
                .playback
                .context
                .as_ref()
                .unwrap()
                .name
                .as_deref(),
            Some("Late-night focus")
        );

        let third = catalog::track(0).uri;
        core.command(
            Command::Play {
                context_uri: PLAYLIST.into(),
                track_uri: Some(third),
            },
            T0,
        )
        .unwrap();
        assert_eq!(title(&core), "Northbound");

        let unknown = Command::Play {
            context_uri: "spotify:playlist:nope".into(),
            track_uri: None,
        };
        assert_eq!(
            core.command(unknown, T0).unwrap_err().reason,
            Reject::Invalid
        );
    }

    #[test]
    fn pausing_keeps_the_position_and_resuming_continues_from_it() {
        let mut core = signed_in();
        play(&mut core);
        core.command(Command::Pause, T0 + 30_000).unwrap();
        let paused = core.state().playback.clone();
        assert_eq!(paused.state, PlayState::Paused);
        assert_eq!(paused.position_at(T0 + 90_000), 30_000);
        core.command(Command::Resume, T0 + 90_000).unwrap();
        assert_eq!(core.state().playback.position_at(T0 + 100_000), 40_000);
    }

    #[test]
    fn tracks_advance_when_they_end_and_the_playlist_stops_after_the_last() {
        let mut core = signed_in();
        play(&mut core);
        // "Night Signals" is 256 s long.
        assert_eq!(core.next_deadline(), Some(T0 + 256_000));
        core.advance(T0 + 256_000 + 10_000);
        assert_eq!(title(&core), "3 AM Radio");
        assert_eq!(core.state().playback.position_at(T0 + 266_000), 10_000);
        // Hours later: all five tracks played, then it stopped.
        core.advance(T0 + 3_600_000);
        assert_eq!(core.state().playback.state, PlayState::Stopped);
        assert_eq!(core.next_deadline(), None);
    }

    #[test]
    fn previous_restarts_a_track_that_played_a_while_else_goes_back() {
        let mut core = signed_in();
        play(&mut core);
        core.command(Command::Next, T0 + 1_000).unwrap();
        assert_eq!(title(&core), "3 AM Radio");
        core.command(Command::Previous, T0 + 11_000).unwrap();
        assert_eq!(title(&core), "3 AM Radio", "10 s in: restart");
        core.command(Command::Previous, T0 + 12_000).unwrap();
        assert_eq!(title(&core), "Night Signals", "1 s in: go back");
    }

    #[test]
    fn volume_is_validated_and_shuffle_picks_another_track() {
        let mut core = signed_in();
        core.command(Command::SetVolume { percent: 80 }, T0)
            .unwrap();
        assert_eq!(core.state().playback.volume, 80);
        let refusal = core
            .command(Command::SetVolume { percent: 101 }, T0)
            .unwrap_err();
        assert_eq!(refusal.reason, Reject::Invalid);

        play(&mut core);
        core.command(Command::SetShuffle { enabled: true }, T0)
            .unwrap();
        let before = title(&core).to_owned();
        core.command(Command::Next, T0 + 1_000).unwrap();
        assert_ne!(title(&core), before);
    }

    #[test]
    fn a_network_loss_counts_down_reconnects_and_resumes_where_it_was() {
        let mut core = signed_in();
        play(&mut core);
        core.inject(Fault::NetworkLoss, T0 + 60_000);
        assert!(matches!(
            core.state().session,
            Session::Failed {
                kind: FailureKind::Network,
                retry_at_ms: Some(at),
                ..
            } if at == T0 + 60_000 + RECONNECT_MS
        ));
        assert_eq!(core.state().playback.state, PlayState::Stopped);
        core.advance(T0 + 60_000 + RECONNECT_MS);
        assert_eq!(core.state().session, Session::Connecting);
        let ready = T0 + 60_000 + RECONNECT_MS + CONNECT_MS;
        core.advance(ready);
        assert_eq!(core.state().session, Session::Ready);
        assert_eq!(core.state().playback.state, PlayState::Playing);
        assert_eq!(title(&core), "Night Signals");
        assert_eq!(core.state().playback.position_at(ready), 60_000);
    }

    #[test]
    fn a_rejected_login_and_a_logout_return_to_pairing_with_nothing_playing() {
        for fault in [Some(Fault::LoginRejected), None] {
            let mut core = signed_in();
            play(&mut core);
            match fault {
                Some(fault) => core.inject(fault, T0 + 1_000),
                None => core.logout(T0 + 1_000),
            }
            assert!(matches!(core.state().session, Session::Pairing { .. }));
            assert_eq!(core.state().playback.track, None);
            assert_eq!(core.state().device, ActiveDevice::None);
        }
    }
}
