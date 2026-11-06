import Starfield from "./starfield";
import { Aurebesh } from "./aurebesh";
import { CmdChip, CopyButton, CvarExplorer } from "./interactive";

const REPO = "https://github.com/VincentMarnier/jampgame-proxy-rs";
const RELEASES = `${REPO}/releases`;

const INSTALL_QUICKSTART = `mv jampgamei386.so jampgame_original.so
# drop the proxy in place of jampgamei386.so, then start as usual`;

const cvars = [
  ["proxy_sv_enable", "1", "Master switch: when 0, detach every hook and forward everything to the original game module (RCON keeps working on the engine's native gate)", "core"],
  ["proxy_sv_muteMode", "1", "Mute behaviour: 0 disabled, 1 drop the player's chat and tell them, 2 silent (their own line is echoed back, nobody else receives it, no notice)", "mute"],
  ["proxy_sv_enableIgnore", "0", "Let any player hide another player's chat from their own client with /ignore and /unignore", "chat"],
  ["proxy_sv_enableBanFile", "0", "File-backed IP ban list: addip / removeip edit proxy_sv_banFile, listip prints it, reloadbans re-reads it, and every connect is checked against it (IPv4 addresses and CIDR ranges)", "bans"],
  ["proxy_sv_banFile", "proxy_bans.txt", "Ban file path, relative to the server's working directory or absolute; ignored while proxy_sv_enableBanFile 0", "bans"],
  ["proxy_sv_enableGuidBan", "0", "File-backed ja_guid ban registry (separate file and cvar from the IP list); rcon ban <player> adds the target's GUID, addguid / removeguid / listguid manage it", "bans"],
  ["proxy_sv_guidBanFile", "proxy_guid_bans.txt", "GUID ban file path, relative to the server's working directory or absolute; ignored while proxy_sv_enableGuidBan 0", "bans"],
  ["proxy_sv_debugLog", "0", "Crash-diagnostics trace: 0 off, 1 suspicious/blocked events, 2 every client command, userinfo and lifecycle event; writes proxy_debug.log", "diagnostics"],
  ["proxy_sv_enableRconCmdCooldown", "0", "Cooldown on RCON commands to avoid flooding", "hardening"],
  ["proxy_sv_maxCallVoteMapRestartValue", "60", "Maximum value allowed on a callvote map_restart", "votes"],
  ["proxy_sv_modelPathLength", "64", "Maximum model path length accepted from clients", "hardening"],
  ["proxy_sv_enableNetStatus", "0", "Per-player network status: netStatus / showNet print the net status table; when 0 the feature is completely inert — no collection, no computation, no table", "observability"],
  ["proxy_sv_enableEndGameStats", "1", "Print personal / global / best-player stats tables at end of game", "stats"],
  ["proxy_sv_antiHpTeller", "0", "Prevent HP tellers usage", "gameplay"],
  ["proxy_sv_minJumpTime", "0", "Minimum time (ms) a player may jump (anti low-jump scripting)", "gameplay"],
  ["proxy_sv_lockTeamsAfterXSecs", "0", "Grace delay in seconds before a round's teams lock (0 = off); teams stay open for that long, and a join in a match cancels its lock for the round", "teams"],
  ["proxy_sv_teamSizeRules", "(empty)", "Per-team-size timelimit / fraglimit / capturelimit overrides, reconciled as the roster changes; per match under TFFA", "teams"],
  ["proxy_tffa_enable", "0", "Run parallel self-organized TFFA matches on one GT_TEAM map: per-match world, teams, chat and scores", "tffa"],
  ["proxy_tffa_maxMatches", "8", "Maximum parallel TFFA matches", "tffa"],
  ["proxy_tffa_maxGroupSize", "12", "Maximum players per TFFA match (both teams combined)", "tffa"],
  ["proxy_sv_spectatorTeamOverlay", "1", "Send the team overlay's HP/armor roster to spectators following a player (TaystJK cg_drawTeamOverlay and the stock team info HUD)", "observability"],
  ["proxy_sv_enableClientPpsLimit", "0", "Per-client packet-rate limiter: the limit is an integer downscale of the client's measured FPS ± proxy_sv_clientPpsFpsMargin, capped by proxy_sv_globalMaxPps; excess packets are dropped evenly through the next second", "network"],
  ["proxy_sv_globalMaxPps", "100", "Global hard ceiling on accepted packets per second per client (0 = no ceiling); the FPS-derived per-client limit is capped to it", "network"],
  ["proxy_sv_clientPpsFpsMargin", "2", "Uncertainty band around the measured FPS used to derive the packet-rate limit: the highest limit over FPS ± margin wins, so the band can only raise it (0 = measured FPS alone)", "network"],
  ["proxy_sv_clientPpsTolerance", "1", "Packets/second of slack added after the FPS band and the global ceiling: 1 accepts 101 packets/s when the derived limit is 100; 0 enforces the exact limit", "network"],
  ["proxy_sv_clientPpsLog", "0", "Log client packet-rate limit transitions (starts using too many packets / back under the limit); works without the limiter", "network"],
  ["proxy_sv_voidTimeNudge", "0", "Neutralise a player's non-zero cl_timeNudge on the server: when the estimated range excludes 0, shift their usercmds' clock by the estimated setting", "network"],
  ["proxy_sv_voidTimeNudgeLog", "0", "Log cl_timeNudge non-zero / back-to-zero transitions; works on its own (reports the shift without applying it)", "network"],
  ["proxy_sv_timeNudgeMargin", "2", "Margin (ms) added to the cl_timeNudge estimate's confidence range: a wider range keeps a small nudge consistent with 0 (more conservative); 0 reports the statistical interval alone", "network"],
  ["proxy_sv_antiMacro", "0", "Freeze a movement or attack channel while a burst of reversals runs faster than a hand can (movement every 1-3 commands or frame-exact wait holds); the view verdict is report-only unless proxy_sv_antiMacroView is on, and the player is never told", "hardening"],
  ["proxy_sv_antiMacroLog", "0", "1 logs anti-macro lock/release transitions, 2 adds the statistics of locked channels, 3 the statistics of every channel with a reversal; works on its own", "diagnostics"],
  ["proxy_sv_antiMacroView", "0", "0 computes and logs the view verdict but never freezes yaw/pitch (hand mouse wiggles are not reliably separable at the wire level); non-zero freezes a locked view", "hardening"],
  ["proxy_sv_antiMacroMinReversals", "4", "Reversals in a run needed for the rate test; raising it is the main false-positive brake", "hardening"],
  ["proxy_sv_antiMacroAxisMaxMeanCmds", "3", "Movement channels: maximum mean interval, in commands, for a rate verdict", "hardening"],
  ["proxy_sv_antiMacroViewMaxMeanMs", "65", "View channels: maximum mean interval, in milliseconds, for a rate verdict", "hardening"],
  ["proxy_sv_antiMacroAttackMaxMeanMs", "60", "Attack channel: maximum mean interval, in milliseconds, for a rate verdict", "hardening"],
  ["proxy_sv_antiMacroMaxHoldSpreadMs", "10", "Hold-spread test: maximum difference in milliseconds between a run's shortest and longest hold (the wait-exactness signature); lower is stricter", "hardening"],
  ["proxy_sv_antiMacroViewStepPercent", "75", "View key-shape test: minimum mean/peak per-command step percentage; 0 disables the shape gate", "hardening"],
  ["proxy_sv_antiMacroViewMinSwing", "364", "View key-shape test: minimum swing between reversals in ANGLE2SHORT units (364 ≈ 2°); 0 removes the floor", "hardening"],
  ["proxy_sv_maxFov", "0", "Maximum cg_fov value a viewer can benefit from: players outside the corresponding view frustum are hidden for that viewer while their footsteps, swings and saber hum keep playing (0 = off)", "fov"],
  ["proxy_sv_maxFovAspect", "1.7777778", "Screen aspect used to turn proxy_sv_maxFov into the rendered view frustum (16:9 default, matching OpenJK/TaystJK's cg_fovAspectAdjust 1; 1.3333334 for 4:3/stock clients, higher for ultrawide)", "fov"],
  ["proxy_sv_maxFovCameraRange", "100", "Third-person camera setback (world units) assumed by the frustum test; 100 matches the stock cg_thirdPersonRange, 0 tests from the player's eye (first-person-strict)", "fov"],
  ["proxy_sv_maxFovMargin", "0", "World-unit margin added to every side of the FOV test volume: a player whose body or drawn saber comes within that distance of the frustum stays visible, so edge contacts do not pop in/out (0 = exact frustum)", "fov"],
];

const hardening = [
  ["Flood & query guards", "getstatus flooding is cut at the ipAuthorize path, q3infoboom-sized getinfo/getstatus requests are refused, and challenge, connect, status and rcon traffic are rate-limited.", "M12 3 2 20h20L12 3zm0 6v5m0 3v.5"],
  ["Parsing & memory bounds", "Length and bounds checks on packet reads (MSG_ReadBits), client commands, userinfo/info, callvotes, gc orders and entity lookups; info-string and Com_Printf overflows are patched.", "M8 6 3 12l5 6M16 6l5 6-5 6M13 4l-2 16"],
  ["Client & content integrity", "Anti-cheat kicks, forcepowers validation, userinfo sanitisation, bugged-model and model-path clamps, download/writeconfig path checks, movement-axis clamps and CR/LF/; injection blocking.", "M12 2 4 6v6c0 5 3.5 8.5 8 10 4.5-1.5 8-5 8-10V6l-8-4zM9 12l2 2 4-4"],
  ["Crash containment", "Guards on gamestate resend and map_restart, vote-kick slot reuse, engine-call/stack-alignment fixes, module pinning across map changes and the nav-graph leak fix.", "M3 12h4l2-6 4 12 2-6h6"],
];

const playerCommands = [
  "tffa",
  "tffa_create [password]",
  "tffa_join <id> [password]",
  "tffa_join 0",
  "tffa_list",
  "/ignore <player>",
  "/unignore <player>",
  "netStatus",
  "showNet",
];

const adminCommands = [
  "mute <clientNumber|playerName>",
  "unmute <clientNumber|playerName>",
  "ban <clientNumber|playerName>",
  "addip <ip[/prefix]>",
  "removeip <ip[/prefix]>",
  "listip",
  "addguid <guid>",
  "removeguid <guid>",
  "listguid",
  "reloadbans",
];

function Icon({ d }) {
  return (
    <svg width="17" height="17" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
      <path d={d} />
    </svg>
  );
}

function Divider() {
  return (
    <div className="rule" aria-hidden="true">
      <Aurebesh text="JAMPGAME PROXY RS" />
    </div>
  );
}

function Code({ title, text, children }) {
  return (
    <div className="codeblock">
      <div className="codeblock-head">
        <span>{title}</span>
        <CopyButton text={text} />
      </div>
      <pre><code>{children}</code></pre>
    </div>
  );
}

function SectionHead({ id, eyebrow, title, lede }) {
  return (
    <>
      <span className="eyebrow">{eyebrow}</span>
      <h2 className="section-title" id={id}>{title}</h2>
      {lede ? <p className="lede">{lede}</p> : null}
    </>
  );
}

export default function Home() {
  return (
    <>
      <Starfield />
      <a className="skip-link" href="#main">Skip to content</a>
      <header className="site">
        <div className="nav-inner">
          <a className="brand" href="#top"><span className="dot">◆</span> jampgame-proxy-rs</a>
          <nav className="site-nav" aria-label="Sections">
            <a href="#how">How</a>
            <a href="#install">Install</a>
            <a href="#features">Features</a>
            <a href="#tffa">TFFA</a>
            <a href="#build">Build</a>
          </nav>
          <a className="nav-cta" href={REPO}>
            <svg width="14" height="14" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true">
              <path d="M12 2a10 10 0 0 0-3.16 19.49c.5.09.68-.22.68-.48v-1.7c-2.78.6-3.37-1.34-3.37-1.34-.45-1.16-1.11-1.47-1.11-1.47-.9-.62.07-.6.07-.6 1 .07 1.53 1.03 1.53 1.03.9 1.52 2.35 1.08 2.92.83.09-.65.35-1.09.63-1.34-2.22-.25-4.56-1.11-4.56-4.94 0-1.1.39-1.99 1.03-2.69-.1-.25-.45-1.27.1-2.64 0 0 .84-.27 2.75 1.02a9.58 9.58 0 0 1 5 0c1.91-1.3 2.75-1.02 2.75-1.02.55 1.37.2 2.39.1 2.64.64.7 1.03 1.6 1.03 2.69 0 3.84-2.34 4.68-4.57 4.93.36.31.68.92.68 1.85V21c0 .27.18.58.69.48A10 10 0 0 0 12 2z" />
            </svg>
            <span>GitHub</span>
          </a>
        </div>
      </header>

      <main id="main">
        {/* ---------- hero — frozen: see docs/decisions/0015-hero-frozen.md ---------- */}
        <div className="hero" id="top">
          <div>
            <span className="status-pill"><span className="pulse" aria-hidden="true" />Anti-cheat &amp; security · QoL features · Saber combat untouched</span>
            <h1>Safer. Smoother.<br />Combat intact.</h1>
            <p className="tagline">
              <strong>jampgame-proxy-rs</strong> is a drop-in replacement for the
              Jedi Academy game module: tighten anti-cheat and security, add
              quality-of-life features for players, and deliberately leave saber
              combat untouched — server-side only, no downloads for players.
            </p>
            <div className="cta-row">
              <a className="btn" href={RELEASES}>Download release</a>
              <a className="btn ghost" href="#install">Install guide</a>
              <a className="btn ghost" href={REPO}>Source</a>
            </div>
            <div className="stat-row" aria-label="Project facts">
              <div className="stat"><b>32</b><span>server cvars</span></div>
              <div className="stat"><b>20+</b><span>always-on guards</span></div>
              <div className="stat"><b>0</b><span>downloads for players</span></div>
            </div>
          </div>
          <div className="terminal" role="img" aria-label="Install quickstart: rename the original module, drop the proxy in its place, start the server">
            <div className="terminal-bar" aria-hidden="true"><i /><i /><i /><span>server gamedir — quickstart</span></div>
            <div className="terminal-body" aria-hidden="true">
              <div><span className="comment"># 1 · keep the original module aside</span></div>
              <div><span className="prompt">$ </span><span className="cmdline">mv jampgamei386.so jampgame_original.so</span></div>
              <div><span className="comment"># 2 · drop the proxy in its place</span></div>
              <div><span className="prompt">$ </span><span className="cmdline">cp jampgamei386.so ./ <span className="comment"># from Releases</span></span></div>
              <div><span className="comment"># 3 · start the server as usual</span></div>
              <div><span className="prompt">$ </span><span className="cmdline">./linuxjampded +exec server.cfg</span></div>
            </div>
            <div className="terminal-foot">
              <CopyButton text={INSTALL_QUICKSTART} />
              <a href="#install" style={{ fontSize: "0.8rem" }}>Full install guide ↓</a>
            </div>
          </div>
        </div>

        <Divider />

        {/* ---------- how ---------- */}
        <section className="block" id="how" aria-labelledby="how-title">
          <SectionHead id="how-title" eyebrow="Architecture" title="How it works" lede="The engine loads the proxy exactly like the original module. The proxy re-opens the pristine module itself and sits in the middle of every call." />
          <div className="pipeline" aria-label="Request flow: engine calls proxy, proxy calls original module">
            <div className="pipe-node">
              <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" aria-hidden="true"><rect x="3" y="4" width="18" height="12" rx="2" /><path d="M8 20h8M12 16v4" /></svg>
              <b>linuxjampded</b>
              <p>2003 i386 engine. Calls <code>vmMain</code> and game traps on <code>jampgamei386.so</code> as always.</p>
            </div>
            <span className="pipe-arrow" aria-hidden="true">→</span>
            <div className="pipe-node mid">
              <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true"><path d="M12 2 4 6v6c0 5 3.5 8.5 8 10 4.5-1.5 8-5 8-10V6l-8-4z" /></svg>
              <b>proxy</b>
              <p>Forwards everything, hooks selected functions to inject features.</p>
            </div>
            <span className="pipe-arrow" aria-hidden="true">→</span>
            <div className="pipe-node">
              <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" aria-hidden="true"><path d="M4 7V5h16v2M9 5v14m6-14v14M4 19h16v2H4z" /></svg>
              <b>jampgame_original.so</b>
              <p>Your pristine game module, renamed aside. Untouched behavior underneath.</p>
            </div>
          </div>
          <Code
            title="call flow"
            text="linuxjampded ──dlopen──▶ jampgamei386.so (proxy) ──dlopen──▶ jampgame_original.so\n                vmMain / syscall traps forwarded both ways,\n                selected functions hooked by the proxy"
          >
            linuxjampded ──dlopen──▶ jampgamei386.so (proxy) ──dlopen──▶ jampgame_original.so{"\n"}
            {"                "}vmMain / syscall traps forwarded both ways,{"\n"}
            {"                "}selected functions hooked by the proxy
          </Code>
          <div className="hook-grid">
            <div className="hook-card"><b>Transparent forward</b><p>All <code>vmMain</code> calls and game traps pass through to the original module both ways.</p></div>
            <div className="hook-card"><b>Surgical hooks</b><p>Entry detours, call-site retargets and inline patches inject features without rewriting the module.</p></div>
            <div className="hook-card"><b>Memory layer reads</b><p>Reads engine memory (<code>serverStatic_t</code> / <code>server_t</code>, cvar slots) for cvars and the net-status table.</p></div>
          </div>
        </section>

        <Divider />

        {/* ---------- install ---------- */}
        <section className="block" id="install" aria-labelledby="install-title">
          <SectionHead id="install-title" eyebrow="Get running" title="Install in three moves" lede="No config migration. Three file operations and the proxy is live." />
          <ol className="steps">
            <li>
              <b>Keep the original module aside</b>
              <p>Rename your server gamedir&apos;s <code>jampgamei386.so</code> to <code>jampgame_original.so</code>.</p>
            </li>
            <li>
              <b>Drop the proxy in its place</b>
              <p>Get <code>jampgamei386.so</code> from <a href={RELEASES}>GitHub Releases</a> (or <a href="#build">build it</a>) and put it where the original was.</p>
            </li>
            <li>
              <b>Start the server as usual</b>
              <p>The proxy loads the original module itself. Set any <code>proxy_*</code> cvar to <code>0</code>/<code>1</code> to tune it — <code>proxy_sv_enable 0</code> detaches every hook while RCON keeps working.</p>
            </li>
          </ol>
        </section>

        <Divider />

        {/* ---------- features ---------- */}
        <section className="block" id="features" aria-labelledby="features-title">
          <SectionHead id="features-title" eyebrow="What you get" title="Hardening, bans, team tools" lede="Security guards run unless proxy_sv_enable is off. Everything else is opt-in per cvar or command; the README has the full detail." />

          <h3 className="sub">Built-in security hardening <span className="count">always on</span></h3>
          <ul className="cards danger hardening">
            {hardening.map(([title, desc, d]) => (
              <li key={title}><b><Icon d={d} />{title}</b><p>{desc}</p></li>
            ))}
          </ul>

          <h3 className="sub">Server cvars <span className="count">43 · filterable</span></h3>
          <CvarExplorer rows={cvars} />
          <p className="note">
            Not available here: <code>proxy_sv_pingFix</code>,{" "}
            <code>proxy_sv_antiWallHack</code>, <code>proxy_sv_sabersFps</code> and{" "}
            <code>proxy_sv_disableKillCmd</code> from the original C++ proxy.
          </p>

          <h3 className="sub">Feature highlights</h3>
          <ul className="cards">
            <li>
              <b>Team control</b>
              <p>
                <code>proxy_sv_lockTeamsAfterXSecs</code> keeps teams open for a
                grace delay, then locks them at their current size (a join in a
                match cancels its lock). <code>proxy_sv_teamSizeRules</code> sets
                timelimit, fraglimit and capturelimit per roster size.
              </p>
            </li>
            <li>
              <b>Chat moderation</b>
              <p>
                <code>rcon mute</code> / <code>unmute</code> drop a player&apos;s
                chat, with a silent mode — <code>proxy_sv_muteMode 2</code>. Any
                player can hide someone&apos;s chat for themselves with{" "}
                <code>/ignore</code>.
              </p>
            </li>
            <li>
              <b>IP &amp; GUID bans</b>
              <p>
                File-backed lists accept IPv4 addresses and CIDR ranges through{" "}
                <code>proxy_sv_enableBanFile</code>; a separate{" "}
                <code>ja_guid</code> registry deters known installs through{" "}
                <code>proxy_sv_enableGuidBan</code>. <code>ban &lt;player&gt;</code>{" "}
                does both and kicks, and a cloud/VPN blocklist ships with the
                source.
              </p>
            </li>
            <li>
              <b>Network tools</b>
              <p>
                <code>netStatus</code> prints ping, RTT, jitter, FPS, packets/s,
                estimated <code>cl_timeNudge</code>, snaps and rate. Optional: a
                per-client packet-rate limiter and neutralising a non-zero{" "}
                <code>cl_timeNudge</code>, both with a tunable FPS margin,
                packet tolerance and time-nudge margin.
              </p>
            </li>
            <li>
              <b>Anti-macro guard</b>
              <p>
                <code>proxy_sv_antiMacro</code> freezes a movement or attack
                channel when its reversals run faster than a hand can; the view
                verdict is report-only unless <code>proxy_sv_antiMacroView 1</code>.
                Per-channel rate, spread and shape thresholds are configurable,
                the last input is held, not zeroed, and the player is never told.
              </p>
            </li>
            <li>
              <b>FOV cap</b>
              <p>
                <code>proxy_sv_maxFov</code> hides players outside the cap per
                viewer, sampling the whole body and a drawn saber.{" "}
                <code>proxy_sv_maxFovMargin</code> softens the screen border;
                spectators are exempt and sounds keep playing.
              </p>
            </li>
            <li>
              <b>Scoreboards &amp; spectating</b>
              <p>
                TaystJK clients get a live, server-authoritative K/D as a 15th
                scoreboard int. Spectators following a player get the stock team
                overlay with live HP and armor.
              </p>
            </li>
            <li>
              <b>Crash diagnostics</b>
              <p>
                <code>proxy_sv_debugLog</code> traces suspicious events or every
                client command to <code>proxy_debug.log</code>, flushed before
                each command so the last line survives a crash.
              </p>
            </li>
          </ul>
          <h3 className="sub">Player commands <span className="count">tap to copy</span></h3>
          <div className="cmds">
            {playerCommands.map((c) => (
              <CmdChip key={c} cmd={c} />
            ))}
          </div>
          <p className="note">
            Typed in the game console; <code>tffa</code> is a shortcut for{" "}
            <code>tffa_list</code>. All player commands are listed here.
          </p>
          <h3 className="sub">Admin commands <span className="count">rcon / console</span></h3>
          <div className="cmds">
            {adminCommands.map((c) => (
              <CmdChip key={c} cmd={c} />
            ))}
          </div>
          <p className="note">
            Run on the dedicated server console, or prefix with <code>rcon</code>{" "}
            from the game console. <code>ban</code> adds the target&apos;s IP and
            GUID (when those registries are enabled) and kicks;{" "}
            <code>reloadbans</code> re-reads both ban files.
          </p>
          <p className="note">
            Full command reference, cvar behaviour and edge cases:{" "}
            <a href={`${REPO}#features`}>README on GitHub</a>.
          </p>
        </section>

        {/* ---------- TFFA ---------- */}
        <section className="block" id="tffa" aria-labelledby="tffa-title">
          <SectionHead id="tffa-title" eyebrow="Parallel play" title="TFFA: many matches, one map" lede="With proxy_tffa_enable 1 (GT_TEAM only), players run parallel matches on one map — separate teams, worlds, chat and scores. Passwords protect a match&apos;s teams without closing spectating." />
          <div className="lifecycle" aria-label="TFFA match lifecycle">
            <div><b><i>1</i>Create</b><code>tffa_create [password]</code> opens a match, joins its smaller side and protects its teams.</div>
            <div><b><i>2</i>Join</b><code>tffa_join &lt;id&gt; [password]</code> switches mid-game; <code>tffa_join 0</code> returns to main. Without the password a protected match is spectate-only.</div>
            <div><b><i>3</i>Fight</b>Damage, snapshots, world entities, spawns, items and the kill feed are isolated per match.</div>
            <div><b><i>4</i>End</b>The fraglimit is shared, with the base game&apos;s one-second damage-void delay; tied matches at the timelimit play sudden death.</div>
          </div>
          <div className="hook-grid">
            <div className="hook-card"><b>Own world per match</b><p>Doors, plats, triggers and map items are duplicated; runtime spawns belong to their creator. Maps with NPC or vehicle actors take no parallel matches.</p></div>
            <div className="hook-card"><b>Survives map_restart</b><p>Players return to their match and team, passwords included, with the world rebuilt and scores reset. A full map change starts everyone in main.</p></div>
            <div className="hook-card"><b>Closes cleanly</b><p>An emptied created match closes itself. <code>proxy_tffa_enable 0</code> moves created-match players to spectator with a message.</p></div>
          </div>
          <ul className="tight">
            <li><code>tffa_create [password]</code> / <code>tffa_join &lt;id&gt; [password]</code> / <code>tffa_join 0</code> / <code>tffa_list</code> work while playing: travel puts you in spectator first, then you re-pick a team with <code>/team red|blue</code>. All player commands are listed above.</li>
            <li>A password only gates joining a team — anyone can spectate without it, and <code>tffa_list</code> marks the match <code>[password]</code>. <code>tffa_join &lt;id&gt; &lt;password&gt;</code> unlocks a match you are already in, including one promoted to the main slot. Passwords are case-sensitive, up to 32 characters, and redacted from the log.</li>
            <li>Auto team picks balance the match being joined; team lock and team-size rules use per-match rosters, and a rule applies only when every populated match is the same <code>N vs N</code>.</li>
            <li>A match that hits the fraglimit plays one second with damage void, then moves its players to spectator and remembers its group for the next <code>map_restart</code>. A full map change drops it; the restart defers proxy pushes over frames so every client is retained.</li>
            <li>Global chat is server-wide; team chat stays in its match, and spectator chat reaches the spectators of every match. The scoreboard groups other matches as spectators and takes the header and scores from your own match (HUD team scores stay global).</li>
            <li>Spawn points are shared, but occupancy is judged per match, so a match never drops players onto another match&apos;s bodies.</li>
          </ul>
        </section>

        {/* ---------- build ---------- */}
        <section className="block" id="build" aria-labelledby="build-title">
          <SectionHead id="build-title" eyebrow="From source" title="Build it yourself" lede="The artifact is a 32-bit (i686) Linux shared object — the containerised dev environment is the recommended route. From rust/:" />
          <Code title="rust/" text="./dev.sh sh ./scripts/build.sh      # -▶ ./jampgamei386.so">
            ./dev.sh sh ./scripts/build.sh      # -▶ ./jampgamei386.so
          </Code>
          <div className="prose">
            <p>
              Full build, test and run instructions (end-to-end engine test, persistent bot
              server) are in <code>rust/README.md</code> on <a href={REPO}>GitHub</a>. Every
              push to <code>main</code> is published
              as a GitHub Release with the <code>.so</code> attached.
            </p>
          </div>
        </section>
      </main>

      <footer className="site">
        <div className="foot-inner">
          <Divider />
          <p className="force">Set up in minutes — server-side only, no player downloads</p>
          <div className="foot-grid">
            <div>
              <b>Project</b>
              <p>Based on <a style={{ display: "inline", padding: 0 }} href="https://github.com/VincentMarnier/jampgame_proxy">jampgame_proxy</a>, extending <a style={{ display: "inline", padding: 0 }} href="https://github.com/Yberion/JKA_YBEProxy">JKA_YBEProxy</a> — a server add-on for the Jedi Academy dedicated server.</p>
            </div>
            <div>
              <b>Site</b>
              <a href="#how">How it works</a>
              <a href="#install">Install</a>
              <a href="#features">Features</a>
              <a href="#tffa">Parallel TFFA</a>
              <a href="#build">Build</a>
            </div>
            <div>
              <b>Upstream</b>
              <a href={REPO}>Source on GitHub</a>
              <a href={RELEASES}>Releases (.so)</a>
              <a href={`${REPO}/issues`}>Issues &amp; PRs</a>
            </div>
          </div>
          <p className="foot-base">
            <a className="to-top" href="#top">↑ Back to top</a>
          </p>
        </div>
      </footer>
    </>
  );
}
