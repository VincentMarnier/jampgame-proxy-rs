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
  ["proxy_sv_enableClientPpsLimit", "0", "Per-client packet-rate limiter: the limit is an integer downscale of the client's measured FPS ± 2, capped by proxy_sv_globalMaxPps; excess packets are dropped evenly through the next second", "network"],
  ["proxy_sv_globalMaxPps", "100", "Global hard ceiling on accepted packets per second per client (0 = no ceiling); the FPS-derived per-client limit is capped to it", "network"],
  ["proxy_sv_clientPpsLog", "0", "Log client packet-rate limit transitions (starts using too many packets / back under the limit); works without the limiter", "network"],
  ["proxy_sv_voidTimeNudge", "0", "Neutralise a player's non-zero cl_timeNudge on the server: when the estimated range excludes 0, shift their usercmds' clock by the estimated setting", "network"],
  ["proxy_sv_voidTimeNudgeLog", "0", "Log cl_timeNudge non-zero / back-to-zero transitions; works on its own (reports the shift without applying it)", "network"],
];

const hardening = [
  ["Flood guard", "getstatus flood protection (ipAuthorize / DDoS removal)", "M3 12a9 9 0 1 0 9 9c-3.5 0-6-1.5-8-4l-1 1zM12 8v4l2.5 2.5"],
  ["Exploit guard", "q3infoboom exploit guard", "M12 3 2 20h20L12 3zm0 6v5m0 3v.5"],
  ["Model clamp", "Bugged-model force and model path length clamp", "M4 7V5h16v2M9 5v14m6-14v14M4 19h16v2H4z"],
  ["Anti-cheat", "Anti-cheat kicks and forcepowers validation", "M12 2 4 6v6c0 5 3.5 8.5 8 10 4.5-1.5 8-5 8-10V6l-8-4zM9 12l2 2 4-4"],
  ["Injection block", "CR/LF and ; command-injection blocking in client messages and chat", "M5 5l14 14M7 17a4 4 0 1 0 6-5M17 7a4 4 0 1 0-6 5"],
  ["Download check", "Download path validation", "M12 3v12m0 0 4-4m-4 4-4-4M4 17v3h16v-3"],
];

const playerCommands = [
  "tffa_create",
  "tffa_join <id>",
  "tffa_leave",
  "tffa_list",
  "netStatus",
  "showNet",
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
        {/* ---------- hero ---------- */}
        <div className="hero" id="top">
          <div>
            <span className="status-pill"><span className="pulse" aria-hidden="true" />Exploit protection · Team tools · Parallel matches</span>
            <h1>Safer servers.<br />More matches.</h1>
            <p className="tagline">
              <strong>jampgame-proxy-rs</strong> protects your Jedi Academy server
              from exploits and cheats, adds team controls, an RCON player mute,
              spectator HP overlays and live K/D on TaystJK scoreboards, and lets
              more players fight at once — server-side only, no downloads for
              players.
            </p>
            <div className="cta-row">
              <a className="btn" href={RELEASES}>Download release</a>
              <a className="btn ghost" href="#install">Install guide</a>
              <a className="btn ghost" href={REPO}>Source</a>
            </div>
            <div className="stat-row" aria-label="Project facts">
              <div className="stat"><b>21</b><span>server cvars</span></div>
              <div className="stat"><b>6</b><span>hardening guards</span></div>
              <div className="stat"><b>8</b><span>parallel TFFA max</span></div>
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
          <SectionHead id="features-title" eyebrow="What you get" title="Hardening, team tools, network control" lede="Security guards run unless the master switch proxy_sv_enable is off. Everything else is opt-in per cvar or command." />

          <h3 className="sub">Built-in security hardening <span className="count">always on</span></h3>
          <ul className="cards danger">
            {hardening.map(([title, desc, d]) => (
              <li key={title}><b><Icon d={d} />{title}</b><p>{desc}</p></li>
            ))}
          </ul>

          <h3 className="sub">Server cvars <span className="count">21 · filterable</span></h3>
          <CvarExplorer rows={cvars} />
          <p className="note">
            Not available here: <code>proxy_sv_pingFix</code>,{" "}
            <code>proxy_sv_antiWallHack</code>, <code>proxy_sv_sabersFps</code> and{" "}
            <code>proxy_sv_disableKillCmd</code> from the original C++ proxy.
          </p>

          <h3 className="sub">Round-start team lock</h3>
          <div className="prose">
            <p>
              <code>proxy_sv_lockTeamsAfterXSecs</code> is a grace delay, in
              seconds, before a round&apos;s teams lock (default <code>0</code>,
              off). When a round starts — map load or <code>map_restart</code> —
              teams stay open for that long. A player who joins a team in a match
              during the window cancels that match&apos;s lock for the round. If
              nobody joins, each team is capped at its current size when the window
              expires, provided the roster is even at that moment (a{" "}
              <code>2 vs 2</code> locks at 2; an uneven <code>2 vs 3</code> is left
              alone). A cap is a reservation: it does not shrink when players
              leave, and a join to a full, locked team is refused (an admin&apos;s{" "}
              <code>forceteam</code> is subject to the same cap). With parallel
              TFFA the lock runs per match: only matches that existed at the round
              start are eligible, and a join in one match cancels only that
              match&apos;s lock.
            </p>
          </div>
          <p className="note">
            Upgrading? <code>proxy_sv_lockTeams</code> is now{" "}
            <code>proxy_sv_lockTeamsAfterXSecs</code> and its meaning changed: the
            value is a grace delay in seconds, not a minimum team size. Old configs
            need the new name; <code>0</code> still means off.
          </p>

          <h3 className="sub">Per-team-size rules</h3>
          <div className="prose">
            <p>
              <code>proxy_sv_teamSizeRules</code> overrides limits for the size the round actually
              started at, independently of <code>proxy_sv_lockTeamsAfterXSecs</code>. Rules are separated by
              spaces/commas as <code>size:timelimit:fraglimit:capturelimit</code> (empty or{" "}
              <code>-</code> leaves a limit untouched). No exact rule? The nearest{" "}
              <em>smaller</em> defined size applies; below every defined size the server&apos;s
              original limits are restored. Limits reconcile every frame as the roster changes.
              With parallel TFFA the rules follow the matches being played: empty matches are
              ignored, and a rule applies only when every populated match is the same{" "}
              <code>N vs N</code> (two <code>2 vs 2</code> matches use the <code>2</code> rule,
              not <code>4</code>).
            </p>
          </div>
          <Code title="server.cfg" text={'set proxy_sv_teamSizeRules "2:20:30:5,3:15:40:0"'}>
            set proxy_sv_teamSizeRules &quot;2:20:30:5,3:15:40:0&quot;
          </Code>

          <h3 className="sub">TaystJK K/D scoreboard <span className="count">no cvar</span></h3>
          <div className="prose">
            <p>
              While <code>proxy_sv_enable</code> is <code>1</code>, the proxy advertises{" "}
              <code>taystJKinfo 128</code> and appends each player&apos;s death count as a 15th
              int to the <code>scores</code> command — but only to a client whose TaystJK cgame
              advertises support (<code>taystJKcgflags</code> bit 0). Updated{" "}
              <a href="https://github.com/taysta/TaystJK">TaystJK</a> clients show a live,
              server-authoritative K/D on the tab scoreboard. Stock JKA, JA+, jaPRO and older
              TaystJK builds keep the pristine 14-int message, so mixed rosters work and nobody
              else&apos;s scoreboard breaks.
            </p>
          </div>

          <h3 className="sub">Player mute <span className="count">rcon</span></h3>
          <div className="prose">
            <p>
              <code>rcon mute &lt;player&gt;</code> silences a player&apos;s chat and{" "}
              <code>rcon unmute &lt;player&gt;</code> gives it back.{" "}
              <code>&lt;player&gt;</code> is the client number <code>status</code>{" "}
              shows (<code>rcon mute 0</code>) or any part of the name — case- and
              colour-code-insensitive, exact first, then a prefix, then the fragment
              wherever it sits (<code>rcon mute ade</code> finds{" "}
              <i>Darth Vader</i>). Names with spaces work as typed; a fragment shared
              by several players lists the candidates, and a bare{" "}
              <code>rcon mute</code> prints the usage and who is currently muted.
            </p>
            <p>
              A muted player keeps playing normally: only <code>say</code>,{" "}
              <code>say_team</code>, <code>tell</code> and the voice menu&apos;s{" "}
              <code>voice_cmd</code> are dropped, and they are told they are muted.
              Mutes are per client slot, cleared when the player leaves, kept across a{" "}
              <code>map_restart</code>, and a full map change starts with nobody
              muted. Like every proxy feature they need <code>proxy_sv_enable 1</code>,
              and the same commands work in the dedicated server&apos;s own console.
            </p>
            <p>
              <code>proxy_sv_muteMode</code> selects what a mute does:{" "}
              <code>1</code> (default) drops the four chat commands and tells the
              player they are muted; <code>2</code> is a silent mute — their{" "}
              <code>say</code>/<code>say_team</code>/<code>tell</code> still reach
              the game so their own line appears as usual, but nobody else receives
              it and no notice is sent, while <code>voice_cmd</code> is dropped
              silently; <code>0</code> turns the feature off — <code>mute</code>/
              <code>unmute</code> only answer that it is disabled and no chat is
              filtered. In silent mode the game&apos;s own <code>say:</code> log
              still records the line, so moderation stays auditable.
            </p>
          </div>

          <h3 className="sub">Spectator team overlay <span className="count">default on</span></h3>
          <div className="prose">
            <p>
              With <code>proxy_sv_spectatorTeamOverlay 1</code> (the default), a
              spectator who is following a player receives the same team
              information the players get: name, health, armor, weapon and
              powerups for the followed player&apos;s team, refreshed once per
              second. That is what makes{" "}
              <a href="https://github.com/taysta/TaystJK">TaystJK</a>&apos;s{" "}
              <code>cg_drawTeamOverlay</code> (including the bar styles 5/6) show
              live HP while spectating; the stock game only sends this to
              teammates. It is the stock <code>tinfo</code> message in its usual
              format, so unmodified clients get the stock <code>team info</code>{" "}
              HUD element too — nothing to install client-side.
            </p>
            <p>
              The overlay is drawn while the spectator <b>follows a player</b>; in
              free-fly spectating the cgame hides it, which cannot be changed from
              the server. Switching to a player of the other team brings that
              team&apos;s roster immediately, otherwise it refreshes once per
              second. <code>proxy_sv_spectatorTeamOverlay 0</code> stops the feed.
            </p>
          </div>

          <h3 className="sub" id="netstatus">Net status <span className="count">opt-in</span></h3>
          <div className="prose">
            <p>
              With <code>proxy_sv_enableNetStatus 1</code>, the{" "}
              <code>netStatus</code> and <code>showNet</code> commands print a
              per-player table (rate-limited to once per 500 ms, bots excluded):
            </p>
          </div>
          <Code
            title="netStatus"
            text={"ping    rtt   jit  fps packets/s     timeNudge snaps   rate name\n---- ------ ----- ---- --------- ------------- ----- ------ ---------------\n   0    0.0   0.0   60        30 [-17.9..-10.5]    20  25000 client0"}
          >
            {"ping    rtt   jit  fps packets/s     timeNudge snaps   rate name\n---- ------ ----- ---- --------- ------------- ----- ------ ---------------\n   0    0.0   0.0   60        30 [-17.9..-10.5]    20  25000 client0"}
          </Code>
          <ul className="tight">
            <li><b>ping / rtt / jit</b> — the scoreboard ping, the round-trip time the server measures itself (the &ldquo;real&rdquo; ping, 0 on the same machine) and its jitter: 0 is rock steady, a flaky line shows a large number.</li>
            <li><b>fps / packets/s</b> — moves per second the player&apos;s game produces, and packets per second it sends, counted from every incoming packet so empty or duplicate packets show up too. A packet bundles several moves, so packets/s follows <code>cl_maxpackets</code>; on the same machine packets/s = fps. Over the limiter&apos;s allowance the value turns red: with <code>proxy_sv_enableClientPpsLimit 1</code> it shows the <b>effective</b> rate the limiter lets through, and with the limiter off the red actual rate is followed by the rate that would be enforced (<code>100/73</code> means &ldquo;sending 100, limited to 73&rdquo;).</li>
            <li><b>timeNudge</b> — the player&apos;s <code>cl_timeNudge</code>, which the client never tells the server, so it is <b>estimated</b> and reported as a range: <code>[-17.9..-10.5]</code> means &ldquo;between -17.9 and -10.5&rdquo;, <code>-</code> means not enough data yet. Values are never clamped to ±30 — a reading outside that range is evidence of a modified client. It is white when 0 is inside the range, yellow when 0 is outside it (a confidently non-zero setting) and red when the whole range sits outside the client&apos;s ±30 clamp. With <code>proxy_sv_voidTimeNudge</code> active the column instead shows what the server enforces: <code>0</code> for every player, red on the ones being voided.</li>
            <li><b>snaps / rate / name</b> — snapshots per second sent to the player (capped by the server&apos;s <code>sv_fps</code>), their bandwidth setting, and who they are.</li>
          </ul>

          <h3 className="sub" id="pps">Client packet rate <span className="count">opt-in</span></h3>
          <div className="prose">
            <p>
              <code>proxy_sv_enableClientPpsLimit 1</code> adds a per-client
              incoming packet-rate limiter. A stock client sends at most one
              packet per generated usercmd (one per rendered frame), so the
              legitimate ceiling is its own frame rate: the proxy measures each
              client&apos;s FPS from the moves it processes and derives the limit
              from it, evaluated over the ±2 uncertainty with the highest result
              winning. <code>proxy_sv_globalMaxPps</code> (default{" "}
              <code>100</code>, <code>0</code> = no ceiling) caps it, so the
              accepted rate is an exact <code>1/K</code> fraction of the
              client&apos;s rate and the eviction pattern is a clean &ldquo;drop
              one out of every K packets&rdquo;.
            </p>
            <p>
              Packets above the limit are dropped <b>evenly through the next
              second</b>, never as a burst at its tail. The first second after a
              connect (and after a 2 s silence) only measures, so handshake and
              reconnect bursts are not filtered; a client inside its limit is
              never touched. Being dropped is exactly a lost UDP packet, which the
              netchan and <code>cl_packetdup</code> already tolerate. The feature
              does not require <code>proxy_sv_enableNetStatus</code>, and{" "}
              <code>proxy_sv_enable 0</code> detaches the whole hook.
            </p>
          </div>
          <Code
            title="server.cfg"
            text={"set proxy_sv_enableClientPpsLimit 1\nset proxy_sv_globalMaxPps 100\nset proxy_sv_clientPpsLog 1"}
          >
            set proxy_sv_enableClientPpsLimit 1{"\n"}
            set proxy_sv_globalMaxPps 100{"\n"}
            set proxy_sv_clientPpsLog 1
          </Code>
          <div className="prose">
            <p>
              <code>proxy_sv_clientPpsLog 1</code> logs <b>transitions</b> — one
              line when a client starts using too many packets, one when a
              completed window brings it back under, another if it starts again.
              It works on its own, without the limiter: it reports the verdict the
              limiter <em>would</em> apply, the same one the net status table
              shows.
            </p>
          </div>
          <Code
            title="log"
            text={"----- proxy-rs: pps over limit: rate=100 limit=62 fps=60 window_ms=1000 client=2 name=\"...\" adr=\"...\"\n----- proxy-rs: pps back under limit: rate=40 limit=62 fps=60 window_ms=1000 client=2 name=\"...\" adr=\"...\""}
          >
            {"----- proxy-rs: pps over limit: rate=100 limit=62 fps=60 window_ms=1000 client=2 name=\"...\"\n----- proxy-rs: pps back under limit: rate=40 limit=62 fps=60 window_ms=1000 client=2 name=\"...\""}
          </Code>

          <h3 className="sub" id="timenudge">Time nudge neutralisation <span className="count">opt-in</span></h3>
          <div className="prose">
            <p>
              <code>cl_timeNudge</code> shifts the clock a player&apos;s game
              stamps into its moves — a negative value makes the client run
              &ldquo;ahead&rdquo; of the server. It is never transmitted, but the
              net status estimator recovers it. With{" "}
              <code>proxy_sv_voidTimeNudge 1</code>, the proxy uses that estimate
              to put the game back on the neutral clock: when the reported range{" "}
              <b>excludes 0</b>, every usercmd&apos;s <code>serverTime</code> is
              shifted by the estimated setting before the game processes it. The
              client keeps its own setting; the server stops accepting the timing
              advantage.
            </p>
            <p>
              The shift is the rounded estimate, clamped to ±30 ms (the release
              client&apos;s own <code>cl_timeNudge</code> clamp) — a reading
              beyond that is left uncorrected rather than amplified. It engages
              only after two consecutive refreshed ranges exclude 0 (about one
              second) and needs the ~5 s estimation window to settle; a range that
              contains 0 disengages immediately. The applied shift moves by 1 ms
              per command, so engaging or disengaging never drops a move. It needs{" "}
              <code>proxy_sv_enable 1</code>, not{" "}
              <code>proxy_sv_enableNetStatus</code>.
            </p>
            <p>
              <code>proxy_sv_voidTimeNudgeLog 1</code> logs the transitions and
              works on its own: with the feature off it still reports the shift it{" "}
              <em>would</em> apply, without rewriting any command. A client that
              stays on the same side of zero is silent.
            </p>
          </div>
          <Code
            title="log"
            text={"----- proxy-rs: timeNudge non-zero: estimate=[-17.0..-11.1] shift=-14 client=2 name=\"...\" adr=\"...\"\n----- proxy-rs: timeNudge back to zero: estimate=[-1.2..3.1] client=2 name=\"...\" adr=\"...\""}
          >
            {"----- proxy-rs: timeNudge non-zero: estimate=[-17.0..-11.1] shift=-14 client=2 name=\"...\" adr=\"...\"\n----- proxy-rs: timeNudge back to zero: estimate=[-1.2..3.1] client=2 name=\"...\" adr=\"...\""}
          </Code>

          <h3 className="sub" id="debuglog">Crash diagnostics <span className="count">opt-in</span></h3>
          <div className="prose">
            <p>
              <code>proxy_sv_debugLog</code> turns on a trace meant to identify a
              player who crashes the server. Set it at runtime with{" "}
              <code>rcon proxy_sv_debugLog 2</code> (or permanently with{" "}
              <code>+set proxy_sv_debugLog 1</code>) and reproduce the crash:{" "}
              <code>1</code> logs suspicious and blocked events — blocked
              commands, forced userinfo/model/forcepowers, anti-cheat kicks, rcon
              attempts (password redacted), rejected downloads, oversized{" "}
              <code>getstatus</code>/<code>getinfo</code> queries and malformed
              client messages; <code>2</code> adds every client command with its
              full text and length, every <code>userinfo</code> update, and
              connect/begin/disconnect events.
            </p>
            <p>
              Each line carries the server time and wall clock, the client slot,
              name and address; client bytes are escaped so a malicious command
              cannot forge log lines. Lines are written and flushed to{" "}
              <code>proxy_debug.log</code> in the server&apos;s working directory{" "}
              <b>before</b> the command is handed to the engine, so the last line
              survives a segmentation fault — that is the command to reproduce
              under gdb. Use it only while investigating: level <code>2</code> is
              intentionally verbose on a busy server. If the file cannot be
              created, the proxy says so once and keeps logging to the console.
            </p>
          </div>
          <Code
            title="log line"
            text={"t=12345 unix_ms=1760000000000 level=SUSPICIOUS event=clientcmd client=2 name=\"...\" adr=\"...\" cmd=\"...\" len=57"}
          >
            {"t=12345 unix_ms=1760000000000 level=SUSPICIOUS event=clientcmd client=2 name=\"...\" adr=\"...\" cmd=\"...\" len=57"}
          </Code>
        </section>

        <Divider />

        {/* ---------- TFFA ---------- */}
        <section className="block" id="tffa" aria-labelledby="tffa-title">
          <SectionHead id="tffa-title" eyebrow="Parallel play" title="TFFA: many matches, one map" lede="With proxy_tffa_enable 1 (GT_TEAM only), players self-organize into parallel matches on one map — each with its own teams, world, chat and scores. No extra servers needed." />
          <div className="lifecycle" aria-label="TFFA match lifecycle">
            <div><b><i>1</i>Create</b><code>tffa_create</code> opens a match and joins its smaller side (red when empty).</div>
            <div><b><i>2</i>Join</b><code>tffa_join &lt;id&gt;</code> switches matches mid-game; <code>tffa_leave</code> or <code>tffa_join 0</code> returns to main.</div>
            <div><b><i>3</i>Fight</b>Damage, snapshots, shields, saber clashes, force sounds, kill feed and items are isolated per match; spawn points are picked without counting another match&apos;s players, so no spawn kills across matches. Global chat stays server-wide; team chat stays in its match.</div>
            <div><b><i>4</i>End</b>Each match shares the server fraglimit: a finished match plays on for one second with damage void before its players go to spectator. Tied matches at the global timelimit play sudden death; when none is tied, the map ends and every player keeps their own match&apos;s final scoreboard.</div>
          </div>
          <div className="hook-grid">
            <div className="hook-card"><b>Own world per match</b><p>Doors, plats, elevators, buttons, triggers and map items are duplicated, and runtime spawns belong to the match that created them. Maps with NPC or vehicle actors take no parallel matches; <code>tffa_create</code> is refused when the entity slots cannot hold another set.</p></div>
            <div className="hook-card"><b>Survives map_restart</b><p>Every player returns to their own match id and team — including a finished match, recreated from its remembered group — with that match&apos;s doors and triggers rebuilt and the scores from zero. The reconnect burst is deferred and spread over frames, so a populated server restarts with every client retained. A full map change starts everyone in main.</p></div>
            <div className="hook-card"><b>Closes cleanly</b><p>An emptied created match closes itself. <code>proxy_tffa_enable 0</code> moves created-match players to spectator with a message; everyone keeps playing in main.</p></div>
          </div>
          <h3 className="sub">Player commands <span className="count">tap to copy</span></h3>
          <div className="cmds">
            {playerCommands.map((c) => (
              <CmdChip key={c} cmd={c} />
            ))}
          </div>
          <ul className="tight">
            <li><code>tffa_create</code> / <code>tffa_join &lt;id&gt;</code> / <code>tffa_leave</code> / <code>tffa_list</code> — create, join, leave, and list parallel TFFA matches (requires <code>proxy_tffa_enable 1</code>). They work while playing, not only from spectator: the travel puts you in spectator first, then you re-pick a team with <code>/team red|blue</code>.</li>
            <li><code>netStatus</code> / <code>showNet</code> — print the per-player net status table: ping, rtt, jitter, fps, pps, timeNudge, snaps, rate and name (see <a href="#netstatus">Net status</a>; requires <code>proxy_sv_enableNetStatus 1</code>).</li>
            <li>Auto team picks (<code>team free</code>, the client auto-join, <code>g_teamAutoJoin</code>) balance the match being joined. <code>proxy_sv_lockTeamsAfterXSecs</code> locks each match against its own roster once the grace delay expires (a join in it cancels its lock; a match created mid-round stays unlocked until the next round), and <code>proxy_sv_teamSizeRules</code> uses per-match sizes — one rule applies when every populated match is the same <code>N vs N</code>.</li>
            <li>A match that reaches the fraglimit keeps playing for one second with its damage void, then its players move to spectator. It remembers who played and for which team, so the next <code>map_restart</code> brings the match back with them — while a full map change drops it.</li>
            <li>At the global timelimit, tied matches keep playing sudden death. When no tied match with players remains, the whole map ends with every match intact: each player reads their own instance&apos;s final scoreboard instead of being sent to main, and the next <code>map_restart</code> resumes the same games.</li>
            <li>Spawn points stay shared coordinates, but their occupancy is judged per match: a match only skips spots its own players stand on, so it never drops players onto another match&apos;s bodies and runs out of spawns.</li>
            <li>The scoreboard lists every match&apos;s players, with the other matches grouped as spectators and the red/blue header plus each score taken from your own match. HUD team scores stay global (per-match HUD would need a client mod). Opening it only re-pushes the player overrides that actually changed, so it no longer costs a multi-KB burst of reliable traffic for viewers with many players in other matches.</li>
            <li>A <code>map_restart</code> on a full server stays inside the engine&apos;s reliable-command window: the proxy&apos;s per-viewer pushes and remembered-match auto-joins are deferred and spread over frames instead of piling onto the reconnect burst, so every client is retained.</li>
          </ul>
        </section>

        <Divider />

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
