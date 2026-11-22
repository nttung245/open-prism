"use client";

import { useMutation } from "@tanstack/react-query";
import { type SubmitEvent, useId, useState } from "react";
import { Button } from "@/components/ui/button";
import { FIELD, Modal } from "@/components/ui/modal";
import { apiFetch } from "@/lib/api/client";
import type { ChangePasswordRequest } from "@/lib/api/types/ChangePasswordRequest";
import { passwordProblem } from "@/lib/auth";

export function ChangePasswordDialog({
  open,
  onClose,
}: {
  open: boolean;
  onClose: () => void;
}) {
  const [current, setCurrent] = useState("");
  const [next, setNext] = useState("");
  const [confirmation, setConfirmation] = useState("");
  const [problem, setProblem] = useState<string | null>(null);
  const hint = useId();
  const change = useMutation({
    mutationFn: (body: ChangePasswordRequest) =>
      apiFetch("/api/auth/password", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify(body),
      }),
  });

  const close = () => {
    setCurrent("");
    setNext("");
    setConfirmation("");
    setProblem(null);
    change.reset();
    onClose();
  };
  const onSubmit = (event: SubmitEvent<HTMLFormElement>) => {
    event.preventDefault();
    const found = passwordProblem(next, confirmation);
    setProblem(found);
    if (!found) change.mutate({ currentPassword: current, newPassword: next });
  };
  const error = problem ?? change.error?.message ?? null;

  return (
    <Modal
      open={open}
      onClose={close}
      title="Change password"
      description="Your other devices are signed out; this one stays signed in."
    >
      {change.isSuccess ? (
        <div className="flex flex-col gap-4">
          <output className="text-sm">Your password is changed.</output>
          <Button variant="primary" className="self-end" onClick={close}>
            Done
          </Button>
        </div>
      ) : (
        <form onSubmit={onSubmit} noValidate className="flex flex-col gap-3">
          <label className="flex flex-col gap-1 text-sm">
            <span className="font-medium">Current password</span>
            <input
              type="password"
              name="current-password"
              autoComplete="current-password"
              value={current}
              onChange={(event) => setCurrent(event.target.value)}
              className={FIELD}
            />
          </label>
          <div className="flex flex-col gap-1 text-sm">
            <label className="flex flex-col gap-1">
              <span className="font-medium">New password</span>
              <input
                type="password"
                name="new-password"
                autoComplete="new-password"
                aria-describedby={hint}
                value={next}
                onChange={(event) => setNext(event.target.value)}
                className={FIELD}
              />
            </label>
            <span id={hint} className="text-xs text-muted">
              At least 12 characters.
            </span>
          </div>
          <label className="flex flex-col gap-1 text-sm">
            <span className="font-medium">New password again</span>
            <input
              type="password"
              name="new-password-again"
              autoComplete="new-password"
              value={confirmation}
              onChange={(event) => setConfirmation(event.target.value)}
              className={FIELD}
            />
          </label>
          {error && (
            <p role="alert" className="text-sm text-danger">
              {error}
            </p>
          )}
          <div className="flex justify-end gap-2">
            <Button onClick={close}>Cancel</Button>
            <Button
              type="submit"
              variant="primary"
              disabled={change.isPending || !current}
            >
              {change.isPending ? "Saving…" : "Change password"}
            </Button>
          </div>
        </form>
      )}
    </Modal>
  );
}
