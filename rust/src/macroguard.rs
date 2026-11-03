//! Anti-macro input guard (`proxy_sv_antiMacro`, `proxy_sv_antiMacroLog`).
//!
//! The guard watches for a channel driven by a config macro instead of a hand.
//! Config scripts move input by **reversing a channel faster than a hand can**:
//! a ground-strafe macro flips a movement axis every few commands, and a
//! turn-key wiggle reverses the view over and over. When a channel sustains such
//! a run, it is frozen (the value of the last command before the run is
//! re-applied) until the reversing stops. Nothing is sent to the player.
//!
//! ## What the server can actually see
//!
//! The engine receives a *summed* command, not the key state: `CL_CreateCmd`
//! adds the movement keys and clamps the result to an `i8`
//! (`codemp/client/cl_input.cpp:1637-1666`), so `+moveleft` and `+moveright`
//! held together arrive as `rightmove == 0`, and a key that is simply not held
//! is indistinguishable from an action. "Opposite keys pressed together" is
//! therefore not observable; the observable form of the same idea is **sign
//! alternation between consecutive commands**.
//!
//! Channels:
//!
//! | channel | observation | payoff |
//! |---|---|---|
//! | `forwardmove`, `rightmove`, `upmove` | sign of the raw `i8` | ground-strafe acceleration: `PM_Accelerate` only adds when the direction changes (`bg_pmove.c:1111-1144`) |
//! | `yaw`, `pitch` | signed 16-bit delta of the raw `ANGLE2SHORT` angle | turn-key view oscillation feeding the saber's per-frame position update |
//! | `attack` | `BUTTON_ATTACK`/`BUTTON_ALT_ATTACK` mask toggling | attack/alt-attack chaining |
//!
//! `usercmd_t.angles` holds `ANGLE2SHORT` values (`cl_input.cpp:2198`,
//! `q_shared.h:1878`), so a delta is the *signed 16-bit* difference of the two
//! commands, shortest-path across the 0/65536 wrap; the client's `delta_angles`
//! offset cancels.
//!
//! ## Runs, not windows
//!
//! A config macro is a **burst**: a contiguous sequence of reversals with short
//! intervals, repeated whenever the player fires the bind — a six-hold script
//! is five reversals, then a long human gap before the next press. Measuring
//! over a fixed time window mixes the burst with those gaps and hides it (the
//! first two implementations did exactly that).
//!
//! So the unit is a **run**: reversals are chained while the gap since the
//! previous reversal is at most [`RUN_MAX_GAP_MS`]; a longer gap starts a new
//! run. Within a run the documented tests are:
//!
//! 1. **rate** — the mean interval is shorter than a hand can sustain, in
//!    commands for movement (the acceleration payoff is per command) and in
//!    milliseconds for view/attack (the payoff is per second);
//! 2. **spread** — the hold lengths, in commands, are confined to a narrow set.
//!    That is what a config `wait` produces, at any rate and on any channel, and
//!    small-range randomisation of the wait counts keeps the set narrow.
//!
//! Four reversals are enough: three intervals at a frame-exact 55 ms (200 fps)
//! is 18 alternations per second, beyond any hand. After a channel has fired,
//! it stays **wary** for [`WARY_MS`]; during that time the next burst fires
//! after only three reversals, so repeated presses are neutralised earlier and
//! earlier while the pattern keeps coming back.
//!
//! `proxy_sv_antiMacroLog 3` prints the observed run figures once per second per
//! client, which is how the thresholds are checked against real play (see
//! `docs/decisions/0021-anti-macro.md`).
//!
//! ## Neutralisation
//!
//! A locked channel is frozen, not zeroed: the command carries the value of
//! the last unlocked command (`rightmove = +127` stays `+127`). That stops the
//! reversal — and with it the acceleration or the wiggle — while leaving the
//! player moving in a direction they already chose. The observer always reads
//! the **raw** command, so a script that keeps reversing keeps the lock alive
//! even though its commands are being rewritten; the lock is released
//! [`RELEASE_MS`] after the last reversal ("forbidden until they stop").
//!
//! The guard is inert unless a cvar asks for it, and it never prints to the
//! player: `proxy_sv_antiMacroLog` writes to the server console only.

use crate::sdk::{MAX_CLIENTS, PITCH, Usercmd, YAW};

/// Channels the guard observes.
pub const CHANNELS: usize = 6;

pub const CH_FORWARD: usize = 0;
pub const CH_RIGHT: usize = 1;
pub const CH_UP: usize = 2;
pub const CH_YAW: usize = 3;
pub const CH_PITCH: usize = 4;
pub const CH_ATTACK: usize = 5;

/// Channel names for the console log (indexed by the `CH_*` constants).
pub const CHANNEL_NAMES: [&str; CHANNELS] = [
    "forwardmove",
    "rightmove",
    "upmove",
    "yaw",
    "pitch",
    "attack",
];

/// `BUTTON_ATTACK` / `BUTTON_ALT_ATTACK` (SDK `q_shared.h:2451,2462`). The
/// attack channel watches the mask, not the individual bits, so holding attack
/// while tapping alt-attack is not a reversal.
const ATTACK_BUTTONS: i32 = 1 | 128;

/// A reversal chained to the previous one within this gap belongs to the same
/// run; a longer gap starts a new run. The reported six-hold script reverses
/// every ~55 ms at 200 fps, and `wait 30` at 60 fps is still ~366 ms, so a
/// macro burst always stays inside the gap while a press-to-press pause (and
/// a human's aim) is far outside it.
const RUN_MAX_GAP_MS: i32 = 200;

/// Reversals that make a run "sustained" for the rate test. Three intervals at
/// a frame-exact pace is already far beyond a hand.
const RUN_MIN_REVERSALS: u16 = 4;

/// Reversals needed for the hold-spread test (three holds).
const SPREAD_MIN_REVERSALS: u16 = 4;

/// A config `wait` gives a few distinct hold counts. A hand's timing spreads
/// further: at 200 fps, ±10 ms is already ±2 commands per hold.
const MAX_HOLD_SPREAD_COMMANDS: u32 = 2;

/// After a channel has fired, the next burst is caught after this many
/// reversals for [`WARY_MS`].
const WARY_MIN_REVERSALS: u16 = 3;
const WARY_MS: i32 = 3_000;

// --- per-payoff thresholds -------------------------------------------------

/// Movement: the acceleration payoff is per command, so the interval is counted
/// in commands. Reversing every 1-3 commands is far beyond a hand; a human
/// half-strafe sits at 5+ commands even at 200 fps. Randomised wait counts do
/// not change the mean.
const AXIS_MAX_MEAN_COMMANDS: u32 = 3;

/// View: milliseconds, not commands — the wiggle's payoff is per second. 65 ms
/// ≈ 15 reversals/s; the reported script is 55 ms at 200 fps (18 % margin). A
/// slower *exact* script is caught by the hold-spread test instead.
const VIEW_MAX_MEAN_MS: i32 = 65;

/// Attack: toggling faster than this (~17/s) is not a human click rate.
const ATTACK_MAX_MEAN_MS: i32 = 60;

// --- structure -------------------------------------------------------------

/// A locked channel is released this long after its last reversal — the
/// "until they stop" part of the rule.
const RELEASE_MS: i32 = 400;

/// A gap larger than this in the command stream (pause, rejoin, clock rewind)
/// forgets every run and lock.
const STALE_GAP_MS: i32 = 400;

/// Minimum `|i8|` for a movement axis to count. Run input is 127, walk input
/// 46; requiring near-run magnitude keeps a walk-speed script (whose payoff is
/// scaled by `PM_CmdScale`, `bg_pmove.c:1177-1199`) out of the detector.
const AXIS_MIN: i32 = 90;

/// Minimum view step, in `ANGLE2SHORT` units (36 ≈ 0.2°). Below it a delta is
/// hand tremor or quantisation noise, not an intentional reversal. At 200 fps
/// the stock `cl_yawspeed 140` turns 127 units per command, so a turn-key
/// script clears the floor with a wide margin up to well past 500 fps.
const ANGLE_MIN_STEP: i32 = 36;

/// With `proxy_sv_antiMacroLog 2`, a locked channel's figures print once per
/// second; level `3` prints every channel with a run (diagnostic).
const DEBUG_PERIOD_MS: i32 = 1_000;

/// Which payoff a channel feeds, selecting the rate test.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Axis,
    View,
    Attack,
}

/// What one command says about a channel.
#[derive(Debug, Clone, Copy)]
enum Observation {
    /// Below the noise floor: says nothing about the reversal, so the state is
    /// left alone (a partially-held turn frame).
    Ignore,
    /// The channel is neutral (key released, walk-speed input): the run ends.
    Neutral,
    /// A directional sample.
    Sign { sign: i8 },
}

/// One channel's run accounting.
#[derive(Debug, Clone, Copy, Default)]
struct ChannelState {
    /// A previous sample exists (so the next one can be a sign change).
    saw: bool,
    /// Sign of the previous sample (`-1` or `1`).
    sign: i8,
    /// Command index of the previous reversal.
    flip_cmd: u32,
    /// `serverTime` of the previous reversal.
    flip_time: i32,
    /// Continuous (unwrapped) view angle, for the swing measure.
    unwrapped: i32,
    /// Current run: reversals, its first sample, and the hold range in commands
    /// (`min_hold_cmds == u32::MAX` while the run has fewer than two holds).
    run_count: u16,
    run_first_cmd: u32,
    run_first_time: i32,
    min_hold_cmds: u32,
    max_hold_cmds: u32,
    swing_min: i32,
    swing_max: i32,
    /// `serverTime` of the most recent reversal (the release deadline).
    last_flip_time: i32,
    /// Until this time, the next burst fires after [`WARY_MIN_REVERSALS`].
    wary_until: i32,
    /// The channel is frozen for this client.
    locked: bool,
    /// Value re-applied while locked (last value written unlocked).
    applied: i32,
}

impl ChannelState {
    /// Feed one command. Returns `true` when the run is machine-like (the
    /// caller sets the lock and the wary deadline).
    fn observe(&mut self, kind: Kind, seq: u32, now: i32, observation: Observation) -> bool {
        let sign = match observation {
            Observation::Ignore => return false,
            Observation::Neutral => {
                self.saw = false;
                self.run_count = 0;
                return false;
            }
            Observation::Sign { sign } => sign,
        };
        if !self.saw {
            self.saw = true;
            self.sign = sign;
            return false;
        }
        if sign == self.sign {
            // Same direction: not a reversal. The run keeps its state; the next
            // reversal's interval decides whether it is still chained.
            return false;
        }

        self.sign = sign;
        let hold_cmds = seq.wrapping_sub(self.flip_cmd);
        let hold_ms = now.wrapping_sub(self.flip_time);
        self.flip_cmd = seq;
        self.flip_time = now;
        self.last_flip_time = now;

        if self.run_count == 0 || hold_ms > RUN_MAX_GAP_MS {
            // Start a fresh run at this reversal. The first hold is measured
            // from the pre-run sample, so it is not part of the hold range.
            self.run_count = 1;
            self.run_first_cmd = seq;
            self.run_first_time = now;
            self.min_hold_cmds = u32::MAX;
            self.max_hold_cmds = 0;
            self.swing_min = self.unwrapped;
            self.swing_max = self.unwrapped;
            return false;
        }

        self.run_count = self.run_count.saturating_add(1);
        self.min_hold_cmds = self.min_hold_cmds.min(hold_cmds);
        self.max_hold_cmds = self.max_hold_cmds.max(hold_cmds);
        self.swing_min = self.swing_min.min(self.unwrapped);
        self.swing_max = self.swing_max.max(self.unwrapped);

        let intervals = (self.run_count - 1) as i32;
        let mean_ms = now.wrapping_sub(self.run_first_time) / intervals;
        let mean_cmds = seq.wrapping_sub(self.run_first_cmd) / intervals as u32;
        let rate = match kind {
            Kind::Axis => mean_cmds <= AXIS_MAX_MEAN_COMMANDS,
            Kind::View => mean_ms <= VIEW_MAX_MEAN_MS,
            Kind::Attack => mean_ms <= ATTACK_MAX_MEAN_MS,
        };
        let sustained = self.run_count >= RUN_MIN_REVERSALS && rate;
        let spread = self.run_count >= SPREAD_MIN_REVERSALS
            && self.max_hold_cmds - self.min_hold_cmds <= MAX_HOLD_SPREAD_COMMANDS;
        let wary = now < self.wary_until && self.run_count >= WARY_MIN_REVERSALS && rate;
        sustained || spread || wary
    }

    /// Mean interval of the current run, milliseconds (0 with fewer than two
    /// reversals).
    fn mean_ms(&self) -> i32 {
        if self.run_count < 2 {
            return 0;
        }
        self.flip_time.wrapping_sub(self.run_first_time) / (self.run_count - 1) as i32
    }

    /// Mean interval of the current run, commands (0 with fewer than two
    /// reversals).
    fn mean_cmds(&self) -> u32 {
        if self.run_count < 2 {
            return 0;
        }
        self.flip_cmd.wrapping_sub(self.run_first_cmd) / (self.run_count - 1) as u32
    }
}

/// Per-client guard state, carried on `ClientEntry`. Dropped on disconnect and
/// rebuilt from scratch by `reset_session` (like every other per-session value).
#[derive(Debug, Clone, Copy)]
pub struct MacroState {
    channels: [ChannelState; CHANNELS],
    /// Previous raw command (the angle delta's other half).
    prev: Usercmd,
    /// A first command has been seen.
    seen: bool,
    /// `serverTime` of the previous command.
    time: i32,
    /// Index of the current command in this client's processed stream.
    seq: u32,
    /// `serverTime` of the last debug line.
    last_debug: i32,
}

impl Default for MacroState {
    fn default() -> Self {
        MacroState {
            channels: [ChannelState::default(); CHANNELS],
            prev: Usercmd::default(),
            seen: false,
            time: 0,
            seq: 0,
            last_debug: 0,
        }
    }
}

impl MacroState {
    /// Whether the guard holds state worth clearing. Lets the disabled hot path
    /// skip the reset write.
    pub fn is_active(&self) -> bool {
        self.seen
    }
}

/// Forget a client's guard state when a new session takes the slot.
pub fn reset_client(client_num: usize) {
    if client_num >= MAX_CLIENTS {
        return;
    }
    crate::state::with_state(|s| {
        s.clients[client_num].macro_guard = MacroState::default();
    });
}

/// Channel statistics for the `proxy_sv_antiMacroLog` diagnostic line.
#[derive(Debug, Clone, Copy)]
pub struct Stats {
    /// Reversals in the current (or most recent) run.
    pub reversals: u16,
    /// Mean interval between reversals, milliseconds.
    pub mean_ms: i32,
    /// Mean interval between reversals, commands.
    pub mean_cmds: u32,
    /// Shortest and longest hold in the run, commands (`0` with one reversal).
    pub min_hold_cmds: u32,
    pub max_hold_cmds: u32,
    /// View swing over the run, `ANGLE2SHORT` units (0 for other channels).
    pub swing: i32,
    /// Whether the channel is currently frozen.
    pub locked: bool,
}

/// The once-per-second diagnostic payload.
#[derive(Debug, Clone, Copy)]
pub struct Debug {
    /// Commands processed for this client so far (proves the feed is alive).
    pub cmds: u32,
    /// Per channel, `Some` when a run exists.
    pub channels: [Option<Stats>; CHANNELS],
}

/// Channel lock transitions this command produced, for the caller to log once
/// the state lock is released (the engine print trap must not run under it).
#[derive(Debug, Clone, Copy, Default)]
pub struct Events {
    pub started: [bool; CHANNELS],
    pub released: [bool; CHANNELS],
    pub debug: Option<Debug>,
}

impl Events {
    pub fn any(&self) -> bool {
        self.started.iter().any(|v| *v) || self.released.iter().any(|v| *v) || self.debug.is_some()
    }
}

/// Observe one raw command and, when `apply` is set, neutralise the channels
/// currently locked.
///
/// The observer always runs (so `proxy_sv_antiMacroLog` can report what the
/// guard *would* do); `apply` only decides whether the command is rewritten.
/// `log_level` is the `proxy_sv_antiMacroLog` level: `0`/`1` no statistics
/// (`1` still logs transitions), `2` the figures for locked channels, `3` the
/// figures for every channel with a run (diagnostic).
pub fn process(state: &mut MacroState, cmd: &mut Usercmd, apply: bool, log_level: i32) -> Events {
    let raw = *cmd;
    let now = raw.server_time;
    let mut events = Events::default();

    let restart = !state.seen
        || now.wrapping_sub(state.time) < 0
        || now.wrapping_sub(state.time) > STALE_GAP_MS;
    if restart {
        // New stream, rewind, or a pause: forget every run and lock. `prev` is
        // seeded with this command so the first angle delta is 0, not a jump
        // from a stale view.
        *state = MacroState {
            prev: raw,
            seen: true,
            time: now,
            ..MacroState::default()
        };
    }

    let prev = state.prev;
    state.prev = raw;
    state.time = now;
    state.seq = state.seq.wrapping_add(1);
    let seq = state.seq;

    for (ch, channel) in state.channels.iter_mut().enumerate() {
        let kind = kind_of(ch);
        // The swing needs a continuous angle: accumulate the signed delta
        // before the reversal check stores it.
        if kind == Kind::View {
            channel.unwrapped = channel.unwrapped.wrapping_add(view_delta(&raw, &prev, ch));
        }
        let observation = sample(ch, &raw, &prev);
        if channel.observe(kind, seq, now, observation) {
            channel.wary_until = now.wrapping_add(WARY_MS);
            if !channel.locked {
                channel.locked = true;
                events.started[ch] = true;
            }
        }
        if channel.locked && now.wrapping_sub(channel.last_flip_time) > RELEASE_MS {
            channel.locked = false;
            events.released[ch] = true;
        }
    }

    for (ch, channel) in state.channels.iter_mut().enumerate() {
        if apply && channel.locked {
            write(ch, cmd, channel.applied);
        } else {
            // Remember what a lock starting on the next command must freeze at.
            channel.applied = read(ch, &raw);
        }
    }

    if log_level >= 2 && now.wrapping_sub(state.last_debug) >= DEBUG_PERIOD_MS {
        state.last_debug = now;
        let debug = state.stats(log_level >= 3);
        // An empty snapshot (nothing locked at level 2) must not produce a log
        // line, or an idle or ordinary player would spam the console.
        if debug.channels.iter().any(|channel| channel.is_some()) {
            events.debug = Some(debug);
        }
    }

    events
}

impl MacroState {
    /// Snapshot of the channels that have a run. Without `full`, only locked
    /// channels are reported, so an idle or ordinary player stays quiet.
    fn stats(&self, full: bool) -> Debug {
        let mut channels = [None; CHANNELS];
        for (ch, channel) in self.channels.iter().enumerate() {
            if channel.run_count == 0 {
                continue;
            }
            if !full && !channel.locked {
                continue;
            }
            channels[ch] = Some(Stats {
                reversals: channel.run_count,
                mean_ms: channel.mean_ms(),
                mean_cmds: channel.mean_cmds(),
                min_hold_cmds: if channel.min_hold_cmds == u32::MAX {
                    0
                } else {
                    channel.min_hold_cmds
                },
                max_hold_cmds: channel.max_hold_cmds,
                swing: if kind_of(ch) == Kind::View {
                    channel.swing_max - channel.swing_min
                } else {
                    0
                },
                locked: channel.locked,
            });
        }
        Debug {
            cmds: self.seq,
            channels,
        }
    }
}

fn kind_of(ch: usize) -> Kind {
    match ch {
        CH_YAW | CH_PITCH => Kind::View,
        CH_ATTACK => Kind::Attack,
        _ => Kind::Axis,
    }
}

/// View delta of `ch` between two commands, shortest path across the
/// `ANGLE2SHORT` wrap.
fn view_delta(raw: &Usercmd, prev: &Usercmd, ch: usize) -> i32 {
    let index = if ch == CH_YAW { YAW } else { PITCH };
    i32::from(raw.angles[index].wrapping_sub(prev.angles[index]) as i16)
}

/// Observation of one channel for `raw`, with `prev` supplying the previous
/// raw angle.
fn sample(ch: usize, raw: &Usercmd, prev: &Usercmd) -> Observation {
    match ch {
        CH_FORWARD => axis_sample(raw.forwardmove),
        CH_RIGHT => axis_sample(raw.rightmove),
        CH_UP => axis_sample(raw.upmove),
        CH_YAW | CH_PITCH => angle_sample(view_delta(raw, prev, ch)),
        CH_ATTACK => {
            let pressed = raw.buttons & ATTACK_BUTTONS != 0;
            Observation::Sign {
                sign: if pressed { 1 } else { -1 },
            }
        }
        _ => Observation::Ignore,
    }
}

fn axis_sample(value: i8) -> Observation {
    let value = i32::from(value);
    if value.abs() < AXIS_MIN {
        // A lifted key, or walk-speed input: no run survives it.
        return Observation::Neutral;
    }
    Observation::Sign {
        sign: value.signum() as i8,
    }
}

fn angle_sample(delta: i32) -> Observation {
    if delta.abs() < ANGLE_MIN_STEP {
        // Below the floor: a partially-held turn frame says nothing.
        return Observation::Ignore;
    }
    Observation::Sign {
        sign: delta.signum() as i8,
    }
}

/// Current value of one channel, in the form [`ChannelState::applied`] keeps.
fn read(ch: usize, cmd: &Usercmd) -> i32 {
    match ch {
        CH_FORWARD => i32::from(cmd.forwardmove),
        CH_RIGHT => i32::from(cmd.rightmove),
        CH_UP => i32::from(cmd.upmove),
        CH_YAW => cmd.angles[YAW],
        CH_PITCH => cmd.angles[PITCH],
        CH_ATTACK => i32::from(cmd.buttons & ATTACK_BUTTONS != 0),
        _ => 0,
    }
}

/// Overwrite one channel with a frozen value.
fn write(ch: usize, cmd: &mut Usercmd, value: i32) {
    match ch {
        CH_FORWARD => cmd.forwardmove = value as i8,
        CH_RIGHT => cmd.rightmove = value as i8,
        CH_UP => cmd.upmove = value as i8,
        CH_YAW => cmd.angles[YAW] = value,
        CH_PITCH => cmd.angles[PITCH] = value,
        CH_ATTACK => {
            if value != 0 {
                cmd.buttons |= ATTACK_BUTTONS;
            } else {
                cmd.buttons &= !ATTACK_BUTTONS;
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Driver that feeds yaw holds at a fixed frame time while keeping time and
    /// angle continuous across segments.
    struct YawDriver {
        state: MacroState,
        time: i32,
        angle: i32,
        index: usize,
    }

    impl YawDriver {
        fn new() -> Self {
            YawDriver {
                state: MacroState::default(),
                time: 0,
                angle: 20_000,
                index: 0,
            }
        }

        /// Hold each direction for the given command counts, rotating by
        /// ±`delta` per command. Returns `(locked, index of the locking command
        /// within this segment)`.
        fn feed(
            &mut self,
            holds: &[u32],
            frame_ms: i32,
            delta: i32,
            apply: bool,
        ) -> (bool, Option<usize>) {
            let start = self.index;
            let mut locked = false;
            let mut lock_at = None;
            for (h, hold) in holds.iter().enumerate() {
                let sign = if h % 2 == 0 { -1 } else { 1 };
                for _ in 0..*hold {
                    self.angle = (self.angle + sign * delta) & 0xFFFF;
                    let mut c = Usercmd {
                        server_time: self.time,
                        ..Usercmd::default()
                    };
                    c.angles[YAW] = self.angle;
                    let e = process(&mut self.state, &mut c, apply, 0);
                    if e.started[CH_YAW] && !locked {
                        locked = true;
                        lock_at = Some(self.index - start);
                    }
                    self.time += frame_ms;
                    self.index += 1;
                }
            }
            (locked, lock_at)
        }

        /// Feed commands at a constant angle for `ms`, letting locks release.
        fn idle(&mut self, ms: i32, frame_ms: i32) {
            let steps = ms / frame_ms;
            for _ in 0..steps {
                let mut c = Usercmd {
                    server_time: self.time,
                    ..Usercmd::default()
                };
                c.angles[YAW] = self.angle;
                process(&mut self.state, &mut c, true, 0);
                self.time += frame_ms;
                self.index += 1;
            }
        }

        fn locked(&self) -> bool {
            self.state.channels[CH_YAW].locked
        }
    }

    /// A command stream that holds `rightmove` at `value` for each one of
    /// `holds` commands at `frame_ms`.
    fn feed_axis_holds(
        state: &mut MacroState,
        holds: &[u32],
        frame_ms: i32,
        value: i8,
    ) -> (bool, Option<usize>) {
        let mut time = 0;
        let mut index = 0usize;
        let mut locked = false;
        let mut lock_at = None;
        for (h, hold) in holds.iter().enumerate() {
            let sign = if h % 2 == 0 { -1 } else { 1 };
            for _ in 0..*hold {
                let mut c = Usercmd {
                    server_time: time,
                    rightmove: sign * value,
                    ..Usercmd::default()
                };
                let e = process(state, &mut c, true, 0);
                if e.started[CH_RIGHT] && !locked {
                    locked = true;
                    lock_at = Some(index);
                }
                time += frame_ms;
                index += 1;
            }
        }
        (locked, lock_at)
    }

    /// A small deterministic "random" hold sequence (no repeating period a
    /// regularity test could exploit).
    fn pseudo_random_holds(seed: u32, count: usize, lo: u32, hi: u32) -> Vec<u32> {
        let mut out = Vec::with_capacity(count);
        let mut s = seed | 1;
        for _ in 0..count {
            s = s.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            out.push(lo + (s >> 16) % (hi - lo + 1));
        }
        out
    }

    /// The operator's bind: six holds, i.e. five reversals, all `wait 10`
    /// (11 commands) at 200 fps.
    const REPORTED_BURST: [u32; 6] = [11, 11, 11, 11, 11, 11];

    #[test]
    fn channel_names_match_the_channel_count() {
        assert_eq!(CHANNEL_NAMES.len(), CHANNELS);
        assert_eq!(YAW, 1);
        assert_eq!(PITCH, 0);
    }

    #[test]
    fn the_reported_bind_fires_on_its_first_press() {
        let mut driver = YawDriver::new();
        let (locked, lock_at) = driver.feed(&REPORTED_BURST, 5, 127, true);
        assert!(locked, "the six-hold bind must fire");
        // Reversals at commands 11, 22, 33, 44: the fourth fires.
        assert_eq!(lock_at, Some(44));
    }

    #[test]
    fn a_later_press_fires_earlier_while_wary() {
        let mut driver = YawDriver::new();
        let (locked, _) = driver.feed(&REPORTED_BURST, 5, 127, true);
        assert!(locked);
        // Stop long enough for the lock to release (but not for the wary
        // deadline), then press again.
        driver.idle(600, 5);
        assert!(
            !driver.locked(),
            "the lock must release when reversing stops"
        );
        let (locked, lock_at) = driver.feed(&REPORTED_BURST, 5, 127, true);
        assert!(locked);
        // Wary: the third reversal of the burst (the first hold already flips
        // the sign left by the previous press) is enough.
        assert_eq!(lock_at, Some(22));
    }

    #[test]
    fn randomised_wait_counts_fire_on_the_first_press() {
        // wait 9..13 inside one burst: the mean is still ~55 ms.
        let mut driver = YawDriver::new();
        let mut holds = pseudo_random_holds(7, 6, 10, 14);
        holds.resize(6, 11);
        let (locked, _) = driver.feed(&holds, 5, 127, true);
        assert!(locked, "randomising the wait counts must not help");
    }

    #[test]
    fn a_slow_exact_burst_fires_on_the_hold_spread() {
        // wait 30 (31 commands, 155 ms) is below the rate threshold but its
        // holds are exact, which is the `wait` signature.
        let mut driver = YawDriver::new();
        let (locked, _) = driver.feed(&[31; 6], 5, 127, true);
        assert!(locked, "a slow but exact view burst must fire");
    }

    #[test]
    fn the_turn_key_lock_freezes_the_view() {
        let mut driver = YawDriver::new();
        let mut frozen = None;
        let mut locked = false;
        for h in 0..6 {
            let sign = if h % 2 == 0 { -1 } else { 1 };
            for _ in 0..11 {
                driver.angle = (driver.angle + sign * 127) & 0xFFFF;
                let mut c = Usercmd {
                    server_time: driver.time,
                    ..Usercmd::default()
                };
                c.angles[YAW] = driver.angle;
                let e = process(&mut driver.state, &mut c, true, 0);
                locked |= e.started[CH_YAW];
                if locked {
                    match frozen {
                        None => frozen = Some(c.angles[YAW]),
                        Some(v) => assert_eq!(c.angles[YAW], v, "a locked view must stop rotating"),
                    }
                }
                driver.time += 5;
            }
        }
        assert!(locked && frozen.is_some());
    }

    #[test]
    fn a_hand_driven_turn_alternation_is_untouched() {
        // A hand mashing turn keys: 4-6 reversals per second, irregular holds.
        let mut driver = YawDriver::new();
        let holds = pseudo_random_holds(3, 10, 33, 60);
        let (locked, _) = driver.feed(&holds, 5, 127, true);
        assert!(!locked, "a hand's reversal rate must not fire");
    }

    #[test]
    fn a_smooth_turn_is_not_an_alternation() {
        let mut driver = YawDriver::new();
        for _ in 0..400 {
            driver.angle = (driver.angle + 127) & 0xFFFF;
            let mut c = Usercmd {
                server_time: driver.time,
                ..Usercmd::default()
            };
            c.angles[YAW] = driver.angle;
            assert!(!process(&mut driver.state, &mut c, true, 0).any());
            driver.time += 5;
        }
    }

    #[test]
    fn a_partially_held_turn_frame_does_not_break_the_run() {
        // Every hold starts with a 1-unit (0.005°) transition frame, below the
        // floor: it must be ignored, not counted as a neutral sample.
        let mut driver = YawDriver::new();
        for h in 0..6 {
            let sign = if h % 2 == 0 { -1 } else { 1 };
            for k in 0..11 {
                let step = if k == 0 { sign } else { sign * 127 };
                driver.angle = (driver.angle + step) & 0xFFFF;
                let mut c = Usercmd {
                    server_time: driver.time,
                    ..Usercmd::default()
                };
                c.angles[YAW] = driver.angle;
                let e = process(&mut driver.state, &mut c, true, 0);
                let _ = e;
                driver.time += 5;
            }
        }
        assert!(driver.locked(), "sub-floor frames must not break the run");
    }

    #[test]
    fn ground_strafe_every_command_locks_and_flattens_the_axis() {
        let mut state = MacroState::default();
        let (locked, lock_at) = feed_axis_holds(&mut state, &[1; 200], 5, 127);
        assert!(locked);
        assert_eq!(lock_at, Some(4), "the 4th reversal fires");

        let mut time = 200 * 5;
        let mut frozen = None;
        for i in 0..40 {
            let raw = if i % 2 == 0 { 127 } else { -127 };
            let mut c = Usercmd {
                server_time: time,
                rightmove: raw,
                ..Usercmd::default()
            };
            process(&mut state, &mut c, true, 0);
            match frozen {
                None => frozen = Some(c.rightmove),
                Some(v) => assert_eq!(c.rightmove, v),
            }
            time += 5;
        }
        assert!(frozen.is_some());
    }

    #[test]
    fn a_randomised_strafe_macro_is_detected() {
        // `wait 1..3` at random: the acceleration payoff needs those short
        // holds, so the mean is what matters, not the pattern.
        let mut state = MacroState::default();
        let holds = pseudo_random_holds(11, 20, 1, 3);
        let (locked, _) = feed_axis_holds(&mut state, &holds, 5, 127);
        assert!(locked);
    }

    #[test]
    fn a_hand_strafe_rhythm_is_untouched() {
        let mut state = MacroState::default();
        let holds = pseudo_random_holds(5, 20, 8, 30);
        let (locked, _) = feed_axis_holds(&mut state, &holds, 5, 127);
        assert!(!locked);
    }

    #[test]
    fn walk_speed_alternation_is_untouched() {
        let mut state = MacroState::default();
        let (locked, _) = feed_axis_holds(&mut state, &[1; 200], 5, 46);
        assert!(!locked, "walk input is not the strafe-macro payoff");
    }

    #[test]
    fn a_lifted_key_breaks_the_run() {
        let mut state = MacroState::default();
        let mut time = 0;
        for _ in 0..200 {
            for value in [127i8, -127, 0] {
                let mut c = Usercmd {
                    server_time: time,
                    rightmove: value,
                    ..Usercmd::default()
                };
                assert!(!process(&mut state, &mut c, true, 0).any());
                time += 5;
            }
        }
    }

    #[test]
    fn a_very_fast_attack_toggle_locks_but_a_human_click_rate_does_not() {
        let mut state = MacroState::default();
        let mut time = 0;
        let mut locked = false;
        for h in 0..10 {
            let pressed = h % 2 == 0;
            for _ in 0..2 {
                let mut c = Usercmd {
                    server_time: time,
                    ..Usercmd::default()
                };
                if pressed {
                    c.buttons = ATTACK_BUTTONS;
                }
                locked |= process(&mut state, &mut c, true, 0).started[CH_ATTACK];
                time += 5;
            }
        }
        assert!(locked, "a frame-rate attack toggle is machine work");

        // A human tapping at ~10/s: holds vary by several commands.
        let mut state = MacroState::default();
        let mut time = 0;
        let holds = pseudo_random_holds(17, 10, 16, 25);
        for (h, hold) in holds.iter().enumerate() {
            let pressed = h % 2 == 0;
            for _ in 0..*hold {
                let mut c = Usercmd {
                    server_time: time,
                    ..Usercmd::default()
                };
                if pressed {
                    c.buttons = ATTACK_BUTTONS;
                }
                assert!(!process(&mut state, &mut c, true, 0).any());
                time += 5;
            }
        }
    }

    #[test]
    fn release_after_the_reversing_stops() {
        let mut driver = YawDriver::new();
        let (locked, _) = driver.feed(&REPORTED_BURST, 5, 127, true);
        assert!(locked);
        let last_flip = driver.time - 11 * 5;

        let mut released_at = None;
        for _ in 0..200 {
            let mut c = Usercmd {
                server_time: driver.time,
                ..Usercmd::default()
            };
            c.angles[YAW] = driver.angle;
            if process(&mut driver.state, &mut c, true, 0).released[CH_YAW] {
                released_at = Some(driver.time);
                break;
            }
            // The view settles: no reversal in this loop.
            driver.time += 5;
        }
        let released_at = released_at.expect("the lock must be released");
        assert!(
            released_at >= last_flip + RELEASE_MS,
            "released too early: {released_at} (last reversal {last_flip})"
        );
    }

    #[test]
    fn yaw_wraparound_is_measured_shortest_path() {
        let mut driver = YawDriver::new();
        driver.angle = 65_000;
        let (locked, _) = driver.feed(&REPORTED_BURST, 5, 127, true);
        assert!(locked, "a wrapped oscillation is the same oscillation");
    }

    #[test]
    fn log_only_mode_reports_but_never_rewrites() {
        let mut driver = YawDriver::new();
        let (locked, _) = driver.feed(&REPORTED_BURST, 5, 127, false);
        assert!(locked, "the observer runs even when nothing is applied");
    }

    #[test]
    fn the_debug_payload_reports_the_run() {
        let mut driver = YawDriver::new();
        let (locked, _) = driver.feed(&REPORTED_BURST, 5, 127, true);
        assert!(locked);
        // Keep the angle constant until a debug line is due; the run stays at
        // its last figures, which is what an operator reads.
        let mut debug = None;
        for _ in 0..400 {
            let mut c = Usercmd {
                server_time: driver.time,
                ..Usercmd::default()
            };
            c.angles[YAW] = driver.angle;
            if let Some(d) = process(&mut driver.state, &mut c, true, 3).debug {
                debug = Some(d);
                break;
            }
            driver.time += 5;
        }
        let debug = debug.expect("a debug line is due after one second");
        assert!(debug.cmds > 100, "the command feed must be visible");
        let stats = debug.channels[CH_YAW].expect("yaw has a run");
        assert_eq!(stats.mean_ms, 55, "11 commands at 5 ms");
        assert!(stats.reversals >= RUN_MIN_REVERSALS);
        assert!(stats.swing > 1_000 && stats.swing < 1_400, "≈6-8° swing");
    }

    #[test]
    fn level_two_stays_quiet_until_a_channel_locks() {
        // Slow, irregular alternation: a hand-like stream that must not fire.
        let holds = pseudo_random_holds(23, 10, 80, 140);
        for level in [2, 3] {
            let mut driver = YawDriver::new();
            let mut lines = 0;
            for (h, hold) in holds.iter().enumerate() {
                let sign = if h % 2 == 0 { -1 } else { 1 };
                for _ in 0..*hold {
                    driver.angle = (driver.angle + sign * 127) & 0xFFFF;
                    let mut c = Usercmd {
                        server_time: driver.time,
                        ..Usercmd::default()
                    };
                    c.angles[YAW] = driver.angle;
                    if process(&mut driver.state, &mut c, true, level)
                        .debug
                        .is_some()
                    {
                        lines += 1;
                    }
                    driver.time += 5;
                }
            }
            if level == 2 {
                assert_eq!(lines, 0, "level 2 must stay quiet while nothing is locked");
            } else {
                assert!(lines > 0, "level 3 reports the run for diagnosis");
            }
        }
    }

    #[test]
    fn a_clock_rewind_forgets_the_lock() {
        let mut driver = YawDriver::new();
        let (locked, _) = driver.feed(&REPORTED_BURST, 5, 127, true);
        assert!(locked);

        let mut rewound = Usercmd {
            server_time: 0,
            rightmove: 127,
            ..Usercmd::default()
        };
        process(&mut driver.state, &mut rewound, true, 0);
        assert!(!driver.locked());

        let mut next = Usercmd {
            server_time: 5,
            rightmove: -127,
            ..Usercmd::default()
        };
        process(&mut driver.state, &mut next, true, 0);
        assert_eq!(next.rightmove, -127, "the stream restarts cleanly");
    }

    #[test]
    fn forward_and_up_are_detected_independently() {
        let mut state = MacroState::default();
        let mut forward = false;
        let mut up = false;
        for i in 0..60 {
            let mut c = Usercmd {
                server_time: i * 5,
                ..Usercmd::default()
            };
            c.forwardmove = if (i / 2) % 2 == 0 { 127 } else { -127 };
            c.upmove = if (i / 3) % 2 == 0 { 127 } else { -127 };
            let e = process(&mut state, &mut c, true, 0);
            forward |= e.started[CH_FORWARD];
            up |= e.started[CH_UP];
        }
        assert!(forward, "the forward axis is watched");
        assert!(up, "the up axis is watched");
    }
}
