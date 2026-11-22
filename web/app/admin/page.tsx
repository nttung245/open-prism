import type { Metadata } from "next";
import { AdminView } from "@/components/admin/admin-view";

export const metadata: Metadata = { title: "Manage the server · Open Prism" };

export default function AdminPage() {
  return <AdminView />;
}
