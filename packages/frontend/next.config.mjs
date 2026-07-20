/**
 * Synchra Next.js config
 *
 * - transpilePackages: ["@synchra/shared"]
 *   Forces Next to compile the workspace package's TS sources
 *   instead of expecting a prebuilt dist/. Lets the frontend import
 *   directly from "@synchra/shared" during dev and prod builds.
 *
 * - publicRuntimeConfig: env-injected values exposed to the
 *   browser via getConfig() / next/config.
 *   Server-only secrets (CIRCLE_API_KEY, CIRCLE_ENTITY_SECRET)
 *   are NOT here — they stay in API routes / server components.
 */

/** @type {import('next').NextConfig} */
const nextConfig = {
  reactStrictMode: true,
  transpilePackages: ["@synchra/shared"],
  publicRuntimeConfig: {
    ARC_TESTNET_RPC: process.env.ARC_TESTNET_RPC || "",
    BACKEND_API_URL: process.env.BACKEND_API_URL || "",
    USYC_ADDRESS: process.env.USYC_ADDRESS || "0xe9185F0c5F296Ed1797AaE4238D26CCaBEadb86C",
  },
};

export default nextConfig;
