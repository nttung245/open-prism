import type { NextConfig } from "next";

const nextConfig: NextConfig = {
  // Self-contained `.next/standalone/server.js` for running next to the Rust API.
  output: "standalone",
  poweredByHeader: false,
  cacheComponents: true,
  partialPrefetching: true,
  turbopack: {
    rules: {
      "*.css": {
        loaders: ["@tailwindcss/turbopack"],
        as: "*.css",
      },
    },
  },
};

export default nextConfig;
