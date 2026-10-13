import Starfield from "./starfield";

const REPO = "https://github.com/VincentMarnier/jampgame-proxy-rs";
const RELEASES = `${REPO}/releases`;

const cvars = [
  ["proxy_sv_enable", "1", "Master switch: when 0, detach every hook and forward everything to the original game module"],
  ["proxy_sv_enableRconCmdCooldown", "0", "Cooldown on RCON commands to avoid flooding"],
  ["proxy_sv_maxCallVoteMapRestartValue", "60", "Maximum value allowed on a callvote map_restart"],
  ["proxy_sv_modelPathLength", "64", "Maximum model path length accepted from clients"],
  ["proxy_sv_enableNetStatus", "0", "Compute per-player network status (netStatus / showNet commands)"],
  ["proxy_sv_enableEndGameStats", "1", "Print personal / global / best-player stats tables at end of game"],
  ["proxy_sv_antiHpTeller", "0", "Prevent HP tellers usage"],
  ["proxy_sv_minJumpTime", "0", "Minimum time (ms) a player may jump (anti low-jump scripting)"],
  ["proxy_sv_lockTeams", "0", "Lock both teams at round-start size when a TDM/CTF round starts even and populated"],
  ["proxy_sv_teamSizeRules", "(empty)", "Per-team-size timelimit / fraglimit / capturelimit overrides"],
  ["proxy_tffa_enable", "0", "Run parallel self-organized TFFA matches on one GT_TEAM map"],
  ["proxy_tffa_maxMatches", "8", "Maximum parallel TFFA matches"],
  ["proxy_tffa_maxGroupSize", "12", "Maximum players per TFFA match (both teams combined)"],
];

const hardening = [
  "getstatus flood protection (ipAuthorize / DDoS removal)",
  "q3infoboom exploit guard",
  "Bugged-model force and model path length clamp",
  "Anti-cheat kicks and forcepowers validation",
  "CR/LF and ; command-injection blocking in client messages and chat",
  "Download path validation",
];

const tffaCommands = ["tffa_create", "tffa_join <id>", "tffa_leave", "tffa_list"];

function Saber({ color = "blue" }) {
  return (
    <div className={`saber saber-${color}`} aria-hidden="true">
      <span className="hilt" />
      <span className="blade" />
    </div>
  );
}

function Code({ children }) {
  return (
    <pre className="codeblock"><code>{children}</code></pre>
  );
}

export default function Home() {
  return (
    <>
      <Starfield />
      <header className="site">
        <nav>
          <a className="brand" href="#top"><span className="dot">◆</span> jampgame-proxy-rs</a>
          <a href="#install">Install</a>
          <a href="#features">Features</a>
          <a href="#build">Build</a>
          <a href={REPO}>GitHub</a>
        </nav>
      </header>
      <main id="top">
        <div className="hero">
          <span className="kicker">A Jedi Academy dedicated-server modification</span>
          <h1>jampgame-proxy-rs</h1>
          <p className="tagline">
            A drop-in Rust proxy for the game module <code>jampgamei386.so</code>:
            it forwards everything to the original module and layers security
            hardening, team tools, and parallel TFFA matches on top.
          </p>
          <div className="cta">
            <a className="btn" href={RELEASES}>Download release</a>
            <a className="btn ghost" href="#install">Install guide</a>
            <a className="btn ghost" href={REPO}>Source</a>
          </div>
        </div>

        <Saber color="blue" />

        <section id="how">
          <h2>How it works</h2>
          <p>
            The engine loads the proxy as any <code>jampgamei386.so</code>.
            The proxy forwards all <code>vmMain</code> calls and game traps to{" "}
            <code>jampgame_original.so</code>, hooks selected engine and game
            functions to inject its features, and reads the engine memory
            layer (<code>serverStatic_t</code> / <code>server_t</code>, cvar
            slots) for cvars and the net-status table.
          </p>
          <Code>
            linuxjampded ──dlopen──▶ jampgamei386.so (proxy) ──dlopen──▶ jampgame_original.so{"\n"}
            {"                "}vmMain / syscall traps forwarded both ways,{"\n"}
            {"                "}selected functions hooked by the proxy
          </Code>
        </section>

        <Saber color="green" />

        <section id="install">
          <h2>Install</h2>
          <ol className="steps">
            <li>
              Rename the original <code>jampgamei386.so</code> of your
              server&apos;s gamedir to <code>jampgame_original.so</code>.
            </li>
            <li>
              Get the proxy from <a href={RELEASES}>GitHub Releases</a> (or
              build it, see below) and put it in place of the original{" "}
              <code>jampgamei386.so</code>.
            </li>
            <li>
              Start the server as usual — the proxy loads the original module
              itself.
            </li>
          </ol>
        </section>

        <Saber color="red" />

        <section id="features">
          <h2>Features</h2>
          <h3>Built-in security hardening</h3>
          <p>
            Active unless the master switch <code>proxy_sv_enable</code> is
            turned off:
          </p>
          <ul className="cards danger">
            {hardening.map((h) => (
              <li key={h}>{h}</li>
            ))}
          </ul>

          <h3>Server cvars</h3>
          <div className="table-wrap">
            <table>
              <thead>
                <tr>
                  <th>cvar</th>
                  <th>default</th>
                  <th>description</th>
                </tr>
              </thead>
              <tbody>
                {cvars.map(([name, def, desc]) => (
                  <tr key={name}>
                    <td><code>{name}</code></td>
                    <td><code>{def}</code></td>
                    <td>{desc}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
          <p>
            The Rust port deliberately drops{" "}
            <code>proxy_sv_pingFix</code>, <code>proxy_sv_antiWallHack</code>,{" "}
            <code>proxy_sv_sabersFps</code> and{" "}
            <code>proxy_sv_disableKillCmd</code> from the original C++ proxy.
          </p>

          <h3>Per-team-size rules</h3>
          <p>
            <code>proxy_sv_teamSizeRules</code> overrides limits for the size
            the round actually started at. Rules are separated by
            spaces/commas as <code>size:timelimit:fraglimit:capturelimit</code>{" "}
            (empty or <code>-</code> leaves a limit untouched):
          </p>
          <Code>set proxy_sv_teamSizeRules &quot;2:20:30:5,3:15:40:0&quot;</Code>
          <p>
            A round with no exact rule falls back to the nearest smaller
            defined size; below every defined size the server&apos;s original
            limits are restored. Limits reconcile every frame as the roster
            changes.
          </p>

          <h3>Parallel TFFA matches</h3>
          <p>
            With <code>proxy_tffa_enable 1</code> (GT_TEAM only), players
            self-organize into parallel matches on the same map — many duels
            of fate, one server. <code>tffa_create</code> opens a match and
            joins it, <code>tffa_join &lt;id&gt;</code> / <code>tffa_leave</code>{" "}
            switch matches and <code>tffa_list</code> shows them.
          </p>
          <ul className="tight">
            <li>Cross-match damage dropped; cross-match players, projectiles, sabers and kill feed hidden.</li>
            <li>Scoreboard shows only the viewer&apos;s match with its own red/blue scores. Chat stays global.</li>
            <li>Every match shares the server&apos;s <code>fraglimit</code> independently; <code>timelimit</code> stays global.</li>
            <li>When a match hits the fraglimit its members move to spectator and may join another match.</li>
          </ul>

          <h3>Client commands</h3>
          <p>Typed in-game by players:</p>
          <div className="cmds">
            {tffaCommands.map((c) => (
              <span className="cmd" key={c}>{c}</span>
            ))}
            <span className="cmd">netStatus</span>
            <span className="cmd">showNet</span>
          </div>
          <ul className="tight">
            <li><code>tffa_create</code> / <code>tffa_join &lt;id&gt;</code> / <code>tffa_leave</code> / <code>tffa_list</code> — create, join, leave, and list parallel TFFA matches (requires <code>proxy_tffa_enable 1</code>).</li>
            <li><code>netStatus</code> / <code>showNet</code> — print the per-player network status table (score, ping, rate, fps, packets, timeNudge, snaps, id, name; requires <code>proxy_sv_enableNetStatus 1</code>).</li>
          </ul>
        </section>

        <Saber color="gold" />

        <section id="build">
          <h2>Build from source</h2>
          <p>
            The artifact is a 32-bit (<code>i686</code>) Linux shared object;
            the recommended route is the containerised dev environment
            (Docker). From <code>rust/</code>:
          </p>
          <Code>./dev.sh sh ./scripts/build.sh      # -▶ ./jampgamei386.so</Code>
          <p>
            Full build, test and run instructions (end-to-end engine test,
            persistent bot server) are in <code>rust/README.md</code> on{" "}
            <a href={REPO}>GitHub</a>. Every push to <code>main</code> is
            version-bumped from conventional commits and published as a GitHub
            Release with the <code>.so</code> attached.
          </p>
        </section>
      </main>
      <footer className="site">
        <Saber color="blue" />
        <p className="force">May the Force serve your packets well</p>
        <p>
          Rust rewrite of{" "}
          <a href="https://github.com/VincentMarnier/jampgame_proxy">jampgame_proxy</a>,
          extending <a href="https://github.com/Yberion/JKA_YBEProxy">JKA_YBEProxy</a>{" "}
          · <a href={REPO}>source</a> · <a href={RELEASES}>releases</a>
        </p>
      </footer>
    </>
  );
}
