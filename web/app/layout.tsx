import type { Metadata } from "next";
import "./globals.css";
import { Providers } from "./providers";

export const metadata: Metadata = {
  title: "Open Prism",
  description:
    "LaTeX editor with AI assistance that works with any LLM provider.",
};

export default function RootLayout({ children }: LayoutProps<"/">) {
  return (
    <html lang="en" className="h-full antialiased">
      <body className="flex h-full min-h-full flex-col">
        <Providers>{children}</Providers>
      </body>
    </html>
  );
}
