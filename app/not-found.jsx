export default function NotFound() {
  return (
    <main style={{ maxWidth: 640, margin: "5rem auto", padding: "0 1.25rem", textAlign: "center", position: "relative", zIndex: 1 }}>
      <p className="eyebrow" style={{ justifyContent: "center", display: "inline-flex" }}>Lost in the Outer Rim</p>
      <h1 className="section-title" style={{ fontSize: "2rem", textAlign: "center" }}>This is not the page you seek</h1>
      <p className="lede" style={{ textAlign: "center", margin: "0.6rem auto 0" }}>
        This site is a single static page. <a href="./">Return to base</a>.
      </p>
    </main>
  );
}
