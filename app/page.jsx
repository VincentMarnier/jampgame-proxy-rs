import Starfield from "./starfield";
import { Aurebesh } from "./aurebesh";
import { CmdChip, CopyButton, CvarExplorer } from "./interactive";

const REPO = "https://github.com/VincentMarnier/jampgame-proxy-rs";
const RELEASES = `${REPO}/releases`;

const INSTALL_QUICKSTART = `mv jampgamei386.so jampgame_original.so
# drop the proxy in place of jampgamei386.so, then start as usual`;

const cvars = [
  ["proxy_sv_enable", "1", "Master switch: when 0, detach every hook and forward everything to the original game module (RCON keeps working on the engine's native gate)", "core"],
  ["proxy_sv_enableRconCmdCooldown", "0", "Cooldown on RCON commands to avoid flooding", "hardening"],
  ["proxy_sv_maxCallVoteMapRestartValue", "60", "Maximum value allowed on a callvote map_restart", "votes"],
  ["proxy_sv_modelPathLength", "64", "Maximum model path length accepted from clients", "hardening"],
  ["proxy_sv_enableNetStatus", "0", "Compute per-player network status (netStatus / showNet commands)", "observability"],
  ["proxy_sv_enableEndGameStats", "1", "Print personal / global / best-player stats tables at end of game", "stats"],
  ["proxy_sv_antiHpTeller", "0", "Prevent HP tellers usage", "gameplay"],
  ["proxy_sv_minJumpTime", "0", "Minimum time (ms) a player may jump (anti low-jump scripting)", "gameplay"],
  ["proxy_sv_lockTeams", "0", "Minimum per-team size for the round-start lock (0 = off); every match locks against its own roster when TFFA is on", "teams"],
  ["proxy_sv_teamSizeRules", "(empty)", "Per-team-size timelimit / fraglimit / capturelimit overrides, reconciled as the roster changes; per match under TFFA", "teams"],
  ["proxy_tffa_enable", "0", "Run parallel self-organized TFFA matches on one GT_TEAM map: per-match world, teams, chat and scores", "tffa"],
  ["proxy_tffa_maxMatches", "8", "Maximum parallel TFFA matches", "tffa"],
  ["proxy_tffa_maxGroupSize", "12", "Maximum players per TFFA match (both teams combined)", "tffa"],
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
              from exploits and cheats, adds team controls and live K/D on TaystJK
              scoreboards, and lets more players fight at once — server-side only,
              no downloads for players.
            </p>
            <div className="cta-row">
              <a className="btn" href={RELEASES}>Download release</a>
              <a className="btn ghost" href="#install">Install guide</a>
              <a className="btn ghost" href={REPO}>Source</a>
            </div>
            <div className="stat-row" aria-label="Project facts">
              <div className="stat"><b>13</b><span>server cvars</span></div>
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
          <SectionHead id="features-title" eyebrow="What you get" title="Hardening, team tools, live K/D" lede="Security guards run unless the master switch proxy_sv_enable is off. Everything else is opt-in per cvar." />

          <h3 className="sub">Built-in security hardening <span className="count">always on</span></h3>
          <ul className="cards danger">
            {hardening.map(([title, desc, d]) => (
              <li key={title}><b><Icon d={d} />{title}</b><p>{desc}</p></li>
            ))}
          </ul>

          <h3 className="sub">Server cvars <span className="count">13 · filterable</span></h3>
          <CvarExplorer rows={cvars} />
          <p className="note">
            Not available here: <code>proxy_sv_pingFix</code>,{" "}
            <code>proxy_sv_antiWallHack</code>, <code>proxy_sv_sabersFps</code> and{" "}
            <code>proxy_sv_disableKillCmd</code> from the original C++ proxy.
          </p>

          <h3 className="sub">Round-start team lock</h3>
          <div className="prose">
            <p>
              <code>proxy_sv_lockTeams</code> is the minimum per-team size that may be
              locked (default <code>0</code>, off). When a round starts — map load or{" "}
              <code>map_restart</code> — with both teams even and each fielding at least
              that many players, every team is capped at its round-start size so the match
              keeps its <code>N vs N</code> shape; an uneven start is left alone.{" "}
              <code>1</code> locks from <code>1 vs 1</code> upwards, <code>2</code> from{" "}
              <code>2 vs 2</code>. A cap is a reservation: it does not shrink when players
              leave, and a join to a full, locked team is refused (an admin&apos;s{" "}
              <code>forceteam</code> is subject to the same cap). With parallel TFFA every
              match locks against its own roster.
            </p>
          </div>
          <p className="note">
            Upgrading? <code>proxy_sv_lockTeams</code> is a minimum size now. If you used{" "}
            <code>1</code> as &ldquo;on&rdquo;, set <code>2</code> to keep the previous{" "}
            <code>2 vs 2</code>-and-up behaviour.
          </p>

          <h3 className="sub">Per-team-size rules</h3>
          <div className="prose">
            <p>
              <code>proxy_sv_teamSizeRules</code> overrides limits for the size the round actually
              started at, independently of <code>proxy_sv_lockTeams</code>. Rules are separated by
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
        </section>

        <Divider />

        {/* ---------- TFFA ---------- */}
        <section className="block" id="tffa" aria-labelledby="tffa-title">
          <SectionHead id="tffa-title" eyebrow="Parallel play" title="TFFA: many matches, one map" lede="With proxy_tffa_enable 1 (GT_TEAM only), players self-organize into parallel matches on one map — each with its own teams, world, chat and scores. No extra servers needed." />
          <div className="lifecycle" aria-label="TFFA match lifecycle">
            <div><b><i>1</i>Create</b><code>tffa_create</code> opens a match and joins its smaller side (red when empty).</div>
            <div><b><i>2</i>Join</b><code>tffa_join &lt;id&gt;</code> switches matches mid-game; <code>tffa_leave</code> or <code>tffa_join 0</code> returns to main.</div>
            <div><b><i>3</i>Fight</b>Damage, snapshots, sabers, kill feed and items are isolated per match. Global chat stays server-wide; team chat stays in its match.</div>
            <div><b><i>4</i>End</b>Each match shares the server fraglimit; tied matches at the global timelimit play sudden death. A <code>map_restart</code> keeps matches.</div>
          </div>
          <div className="hook-grid">
            <div className="hook-card"><b>Own world per match</b><p>Doors, plats, elevators, buttons, triggers and map items are duplicated, and runtime spawns belong to the match that created them. Maps with NPC or vehicle actors take no parallel matches; <code>tffa_create</code> is refused when the entity slots cannot hold another set.</p></div>
            <div className="hook-card"><b>Survives map_restart</b><p>Every player reconnects into their match and team, with that match&apos;s doors and triggers rebuilt and the scores from zero. A full map change starts everyone in main.</p></div>
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
            <li><code>netStatus</code> / <code>showNet</code> — print the per-player network status table (score, ping, rate, fps, packets, timeNudge, snaps, id, name; requires <code>proxy_sv_enableNetStatus 1</code>).</li>
            <li>Auto team picks (<code>team free</code>, the client auto-join, <code>g_teamAutoJoin</code>) balance the match being joined. <code>proxy_sv_lockTeams</code> locks each match against its own round-start roster (a match created mid-round stays unlocked until the next round), and <code>proxy_sv_teamSizeRules</code> uses per-match sizes — one rule applies when every populated match is the same <code>N vs N</code>.</li>
            <li>The scoreboard lists every match&apos;s players, with the other matches grouped as spectators and the red/blue header plus each score taken from your own match. HUD team scores stay global (per-match HUD would need a client mod).</li>
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
