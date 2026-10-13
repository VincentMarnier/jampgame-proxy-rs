import "./globals.css";

export const metadata = {
  title: "jampgame-proxy-rs — JKA dedicated-server proxy",
  description:
    "Drop-in Rust proxy for the Jedi Academy dedicated server game module: security hardening, team tools, and parallel TFFA matches.",
};

export default function RootLayout({ children }) {
  return (
    <html lang="en">
      <body>{children}</body>
    </html>
  );
}
