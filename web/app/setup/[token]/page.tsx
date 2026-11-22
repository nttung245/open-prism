import type { Metadata } from "next";
import { Suspense } from "react";
import { SetupForm } from "./setup-form";

export const metadata: Metadata = {
  title: "Set up your account · Open Prism",
  // The address holds a one-time token: never send it to another site.
  referrer: "no-referrer",
};

export default function SetupPage() {
  return (
    <main className="mx-auto flex w-full max-w-sm flex-1 flex-col justify-center gap-6 px-6 py-16">
      <Suspense>
        <SetupForm />
      </Suspense>
    </main>
  );
}
