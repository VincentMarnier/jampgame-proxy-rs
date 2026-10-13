// Static export for GitHub Pages project site:
// https://vincentmarnier.github.io/jampgame-proxy-rs/
const isPages = process.env.GITHUB_ACTIONS === "true";
const basePath = isPages ? "/jampgame-proxy-rs" : "";

/** @type {import('next').NextConfig} */
const nextConfig = {
  output: "export",
  trailingSlash: true,
  basePath,
  assetPrefix: basePath ? `${basePath}/` : undefined,
  images: { unoptimized: true },
};

export default nextConfig;
