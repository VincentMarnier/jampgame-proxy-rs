import "./globals.css";
import { logoFont, menuFont } from "./fonts";

export const metadata = {
  title: "jampgame-proxy-rs — JKA dedicated-server proxy",
  description:
    "Add-on for the Jedi Academy dedicated server: exploit protection, team controls, and parallel matches.",
};

export default function RootLayout({ children }) {
  return (
    <html lang="en" className={`${logoFont.variable} ${menuFont.variable}`}>
      <body>{children}</body>
    </html>
  );
}
