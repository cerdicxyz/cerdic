import type { Metadata } from "next";
import "./globals.css";

export const metadata: Metadata = {
  title: "Synchra",
  description:
    "Privacy-preserving clearing and leverage layer for Arc — stablecoin-native, agent-driven capital markets.",
};

export default function RootLayout({
  children,
}: Readonly<{
  children: React.ReactNode;
}>) {
  return (
    <html lang="en">
      <body>{children}</body>
    </html>
  );
}
