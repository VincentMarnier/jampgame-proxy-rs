import localFont from "next/font/local";

// Bundled display fonts (SIL OFL 1.1, see OFL-*.txt next to the files).
// next/font/local emits them through _next/static with basePath-aware URLs,
// so they work both locally and under /jampgame-proxy-rs on Pages.
export const logoFont = localFont({
  src: "./fonts/audiowide-latin.woff2",
  variable: "--font-logo",
  weight: "400",
  display: "swap",
});

export const menuFont = localFont({
  src: "./fonts/michroma-latin.woff2",
  variable: "--font-menu",
  weight: "400",
  display: "swap",
});
