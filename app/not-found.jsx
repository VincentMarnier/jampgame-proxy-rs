export default function NotFound() {
  return (
    <main style={{ maxWidth: 640, margin: "5rem auto", padding: "0 1.25rem", textAlign: "center" }}>
      <p style={{ color: "#5ec8ff", letterSpacing: "0.34em", textTransform: "uppercase", fontSize: "0.72rem" }}>
        Lost in the Outer Rim
      </p>
      <h1 style={{ color: "#ffe81f" }}>This is not the page you seek</h1>
      <p style={{ color: "#8b98ab" }}>
        This site is a single static page. <a href="./">Return to base</a>.
      </p>
    </main>
  );
}
