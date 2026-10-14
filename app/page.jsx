import Starfield from "./starfield";
import { CmdChip, CopyButton, CvarExplorer } from "./interactive";

const REPO = "https://github.com/VincentMarnier/jampgame-proxy-rs";
const RELEASES = `${REPO}/releases`;
const VERSION = "v0.3.1";

const INSTALL_QUICKSTART = `mv jampgamei386.so jampgame_original.so
# drop the proxy in place of jampgamei386.so, then start as usual`;

const cvars = [
  ["proxy_sv_enable", "1", "Master switch: when 0, detach every hook and forward everything to the original game module", "core"],
  ["proxy_sv_enableRconCmdCooldown", "0", "Cooldown on RCON commands to avoid flooding", "hardening"],
  ["proxy_sv_maxCallVoteMapRestartValue", "60", "Maximum value allowed on a callvote map_restart", "votes"],
  ["proxy_sv_modelPathLength", "64", "Maximum model path length accepted from clients", "hardening"],
  ["proxy_sv_enableNetStatus", "0", "Compute per-player network status (netStatus / showNet commands)", "observability"],
  ["proxy_sv_enableEndGameStats", "1", "Print personal / global / best-player stats tables at end of game", "stats"],
  ["proxy_sv_antiHpTeller", "0", "Prevent HP tellers usage", "fair play"],
  ["proxy_sv_minJumpTime", "0", "Minimum time (ms) a player may jump (anti low-jump scripting)", "fair play"],
  ["proxy_sv_lockTeams", "0", "Lock both teams at round-start size when a TDM/CTF round starts even and populated", "teams"],
  ["proxy_sv_teamSizeRules", "(empty)", "Per-team-size timelimit / fraglimit / capturelimit overrides", "teams"],
  ["proxy_tffa_enable", "0", "Run parallel self-organized TFFA matches on one GT_TEAM map", "tffa"],
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

function Saber({ color = "blue" }) {
  return (
    <div className={`saber saber-${color}`} aria-hidden="true">
      <span className="hilt" />
      <span className="blade" />
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
            <span className="status-pill"><span className="pulse" aria-hidden="true" />{VERSION} · Rust · i686 .so · drop-in proxy</span>
            <h1>One server.<br />Many duels of fate.</h1>
            <p className="tagline">
              <strong>jampgame-proxy-rs</strong> is a drop-in Rust proxy for the Jedi Academy
              dedicated-server module <code>jampgamei386.so</code> — it forwards everything
              to the original module, then layers <strong>security hardening</strong>,{" "}
              <strong>team tools</strong>, and <strong>parallel TFFA matches</strong> on top.
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

        <Saber color="blue" />

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
              <b>proxy (Rust)</b>
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

        <Saber color="green" />

        {/* ---------- install ---------- */}
        <section className="block" id="install" aria-labelledby="install-title">
          <SectionHead id="install-title" eyebrow="Get running" title="Install in three moves" lede="No engine patching, no config migration. Three file operations and the proxy is live." />
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
              <p>The proxy loads the original module itself. Set any <code>proxy_*</code> cvar to <code>0</code>/<code>1</code> to tune it — <code>proxy_sv_enable 0</code> detaches every hook.</p>
            </li>
          </ol>
        </section>

        <Saber color="red" />

        {/* ---------- features ---------- */}
        <section className="block" id="features" aria-labelledby="features-title">
          <SectionHead id="features-title" eyebrow="What you get" title="Hardening, team tools, telemetry" lede="Security guards run unless the master switch proxy_sv_enable is off. Everything else is opt-in per cvar." />

          <h3 className="sub">Built-in security hardening <span className="count">always on</span></h3>
          <ul className="cards danger">
            {hardening.map(([title, desc, d]) => (
              <li key={title}><b><Icon d={d} />{title}</b><p>{desc}</p></li>
            ))}
          </ul>

          <h3 className="sub">Server cvars <span className="count">13 · filterable</span></h3>
          <CvarExplorer rows={cvars} />
          <p className="note">
            The Rust port deliberately drops <code>proxy_sv_pingFix</code>,{" "}
            <code>proxy_sv_antiWallHack</code>, <code>proxy_sv_sabersFps</code> and{" "}
            <code>proxy_sv_disableKillCmd</code> from the original C++ proxy — they are not available here.
          </p>

          <h3 className="sub">Per-team-size rules</h3>
          <div className="prose">
            <p>
              <code>proxy_sv_teamSizeRules</code> overrides limits for the size the round actually
              started at. Rules are separated by spaces/commas as{" "}
              <code>size:timelimit:fraglimit:capturelimit</code> (empty or <code>-</code> leaves
              a limit untouched). No exact rule? The nearest <em>smaller</em> defined size applies;
              below every defined size the server&apos;s original limits are restored. Limits
              reconcile every frame as the roster changes.
            </p>
          </div>
          <Code title="server.cfg" text={'set proxy_sv_teamSizeRules "2:20:30:5,3:15:40:0"'}>
            set proxy_sv_teamSizeRules &quot;2:20:30:5,3:15:40:0&quot;
          </Code>
        </section>

        <Saber color="purple" />

        {/* ---------- TFFA ---------- */}
        <section className="block" id="tffa" aria-labelledby="tffa-title">
          <SectionHead id="tffa-title" eyebrow="Parallel play" title="TFFA: many matches, one map" lede="With proxy_tffa_enable 1 (GT_TEAM only), players self-organize into parallel matches on the same map — many duels of fate, one server." />
          <div className="lifecycle" aria-label="TFFA match lifecycle">
            <div><b><i>1</i>Create</b><code>tffa_create</code> opens a match and joins it.</div>
            <div><b><i>2</i>Join</b><code>tffa_join &lt;id&gt;</code> switches matches; <code>tffa_list</code> shows them.</div>
            <div><b><i>3</i>Fight</b>Damage, snapshots, sabers, kill feed and scoreboard are isolated per match. Chat stays global.</div>
            <div><b><i>4</i>Fraglimit</b>Hitting the match fraglimit moves members to spectator — free to join another match. Timelimit stays global.</div>
          </div>
          <h3 className="sub">Player commands <span className="count">tap to copy</span></h3>
          <div className="cmds">
            {playerCommands.map((c) => (
              <CmdChip key={c} cmd={c} />
            ))}
          </div>
          <ul className="tight">
            <li><code>tffa_create</code> / <code>tffa_join &lt;id&gt;</code> / <code>tffa_leave</code> / <code>tffa_list</code> — create, join, leave, and list parallel TFFA matches (requires <code>proxy_tffa_enable 1</code>).</li>
            <li><code>netStatus</code> / <code>showNet</code> — print the per-player network status table (score, ping, rate, fps, packets, timeNudge, snaps, id, name; requires <code>proxy_sv_enableNetStatus 1</code>).</li>
            <li>Every match shares the server&apos;s <code>fraglimit</code> independently; <code>proxy_sv_lockTeams</code> / <code>proxy_sv_teamSizeRules</code> stay off while TFFA is on. HUD team scores stay global (per-match HUD would need a client mod).</li>
          </ul>
        </section>

        <Saber color="gold" />

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
              push to <code>main</code> is version-bumped from conventional commits and published
              as a GitHub Release with the <code>.so</code> attached.
            </p>
          </div>
        </section>
      </main>

      <footer className="site">
        <div className="foot-inner">
          <Saber color="blue" />
          <p className="force">May the Force serve your packets well</p>
          <div className="foot-grid">
            <div>
              <b>Project</b>
              <p>Rust rewrite of <a style={{ display: "inline", padding: 0 }} href="https://github.com/VincentMarnier/jampgame_proxy">jampgame_proxy</a>, extending <a style={{ display: "inline", padding: 0 }} href="https://github.com/Yberion/JKA_YBEProxy">JKA_YBEProxy</a> — a drop-in proxy for the Jedi Academy dedicated-server game module.</p>
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
