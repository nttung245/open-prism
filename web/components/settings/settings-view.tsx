"use client";

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  ArrowLeft,
  CircleCheck,
  CircleX,
  KeyRound,
  LoaderCircle,
  Pencil,
  Plug,
  Plus,
  Trash2,
} from "lucide-react";
import Link from "next/link";
import {
  type ReactNode,
  type SubmitEvent,
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { Button, IconButton } from "@/components/ui/button";
import { parseReasoning, REASONING_OPTIONS } from "@/lib/ai/reasoning";
import { apiFetch, apiJson, errorMessage } from "@/lib/api/client";
import type { ModelRef } from "@/lib/api/types/ModelRef";
import type { ProviderKind } from "@/lib/api/types/ProviderKind";
import type { ProviderKindInfo } from "@/lib/api/types/ProviderKindInfo";
import type { ProviderTestResult } from "@/lib/api/types/ProviderTestResult";
import type { ProviderView } from "@/lib/api/types/ProviderView";
import type { Roles } from "@/lib/api/types/Roles";
import { fetchSession, SESSION_KEY } from "@/lib/auth";

const PROVIDERS = ["providers"] as const;
const ROLES = ["roles"] as const;
const DEFAULT_ROLES = ["roles", "defaults"] as const;

/**
 * Whose AI settings a page shows:
 * - `single`: single-user mode, everything as one person;
 * - `mine`: server mode, Settings: shared and personal providers, the person's own models;
 * - `shared`: server mode, the admin page: shared providers and the default models.
 */
export type AiScope = "single" | "mine" | "shared";

const input =
  "w-full rounded-md border border-border bg-background px-2.5 py-1.5 text-sm outline-none focus:border-accent";

export function SettingsView() {
  const session = useQuery({ queryKey: SESSION_KEY, queryFn: fetchSession });
  const server = session.data?.mode === "server";
  return (
    <main className="mx-auto flex w-full max-w-3xl flex-col gap-10 px-6 py-10">
      <header className="flex items-center gap-3">
        <Link
          href="/"
          aria-label="All projects"
          className="inline-flex size-8 items-center justify-center rounded-md text-muted hover:bg-zinc-200/60 hover:text-foreground dark:hover:bg-zinc-800/60"
        >
          <ArrowLeft className="size-4" />
        </Link>
        <h1 className="text-2xl font-semibold tracking-tight">Settings</h1>
      </header>
      {session.error && (
        <p className="text-sm text-danger">{errorMessage(session.error)}</p>
      )}
      {session.data && (
        <AiSettings
          scope={server ? "mine" : "single"}
          isAdmin={session.data.user?.role === "admin"}
        />
      )}
    </main>
  );
}

/** Providers, then the models of each role, for `scope`. */
export function AiSettings({
  scope,
  isAdmin,
}: {
  scope: AiScope;
  isAdmin: boolean;
}) {
  const kinds = useQuery({
    queryKey: ["provider-kinds"],
    queryFn: () => apiJson<ProviderKindInfo[]>("/api/settings/provider-kinds"),
    staleTime: Number.POSITIVE_INFINITY,
  });
  const providers = useQuery({
    queryKey: PROVIDERS,
    queryFn: () => apiJson<ProviderView[]>("/api/settings/providers"),
  });
  // Models found by "Test connection", per provider.
  const [models, setModels] = useState<Record<string, string[]>>({});
  const shown =
    scope === "shared"
      ? providers.data?.filter((provider) => provider.shared)
      : providers.data;

  return (
    <>
      <section className="flex flex-col gap-4">
        <div>
          <h2 className="text-lg font-semibold">
            {scope === "shared" ? "Shared AI providers" : "AI providers"}
          </h2>
          <p className="text-sm text-muted">
            {scope === "shared"
              ? "Everyone on this server can use these. "
              : scope === "mine"
                ? "Shared providers are set up by an admin; add your own key as a personal provider, which only you see and use. "
                : ""}
            Keys are stored on this server and never sent back to the browser.
          </p>
        </div>
        {providers.error && (
          <p className="text-sm text-danger">{errorMessage(providers.error)}</p>
        )}
        <ul className="flex flex-col gap-3">
          {shown?.map((provider) => (
            <ProviderCard
              key={provider.id}
              provider={provider}
              kind={kinds.data?.find((k) => k.kind === provider.kind)}
              badge={
                scope === "mine"
                  ? provider.shared
                    ? "Shared"
                    : "Personal"
                  : undefined
              }
              canManage={scope !== "mine" || !provider.shared || isAdmin}
              onModels={(list) =>
                setModels((current) => ({ ...current, [provider.id]: list }))
              }
            />
          ))}
        </ul>
        {kinds.data && <AddProvider kinds={kinds.data} scope={scope} />}
      </section>

      {shown && (
        <RolesForm
          scope={scope}
          providers={shown}
          models={models}
          onModels={setModels}
        />
      )}
    </>
  );
}

function ProviderCard({
  provider,
  kind,
  badge,
  canManage,
  onModels,
}: {
  provider: ProviderView;
  kind: ProviderKindInfo | undefined;
  badge?: "Shared" | "Personal";
  canManage: boolean;
  onModels: (models: string[]) => void;
}) {
  const queryClient = useQueryClient();
  const [editing, setEditing] = useState(false);
  const [result, setResult] = useState<ProviderTestResult | null>(null);
  const test = useMutation({
    mutationFn: () =>
      apiJson<ProviderTestResult>(
        `/api/settings/providers/${provider.id}/test`,
        {
          method: "POST",
        },
      ),
    onSuccess: (outcome) => {
      setResult(outcome);
      if (outcome.ok) onModels(outcome.models);
    },
  });
  const remove = useMutation({
    mutationFn: () =>
      apiFetch(`/api/settings/providers/${provider.id}`, { method: "DELETE" }),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: PROVIDERS });
      void queryClient.invalidateQueries({ queryKey: ROLES });
    },
  });

  return (
    <li className="rounded-lg border border-border p-4">
      <div className="flex flex-wrap items-center gap-2">
        <div className="min-w-0 flex-1">
          <p className="font-medium">
            {provider.name}
            {badge && (
              <span
                className={`ml-2 rounded px-1.5 py-0.5 align-middle text-xs font-normal ${badge === "Shared" ? "bg-panel text-muted" : "bg-accent/15 text-accent"}`}
              >
                {badge}
              </span>
            )}
          </p>
          <p className="truncate text-xs text-muted">
            {kind?.label ?? provider.kind}
            {(provider.baseUrl ?? kind?.defaultBaseUrl) &&
              ` · ${provider.baseUrl ?? kind?.defaultBaseUrl}`}
            {kind?.needsApiKey &&
              (provider.hasApiKey ? " · key saved" : " · no key")}
          </p>
        </div>
        <Button onClick={() => test.mutate()} disabled={test.isPending}>
          {test.isPending ? (
            <LoaderCircle className="size-4 animate-spin" />
          ) : (
            <Plug className="size-4" />
          )}
          Test connection
        </Button>
        {canManage && (
          <>
            <IconButton label="Edit" onClick={() => setEditing((v) => !v)}>
              <Pencil className="size-4" />
            </IconButton>
            <IconButton
              label="Delete"
              className="hover:text-danger"
              onClick={() => {
                if (window.confirm(`Delete the provider "${provider.name}"?`))
                  remove.mutate();
              }}
            >
              <Trash2 className="size-4" />
            </IconButton>
          </>
        )}
      </div>
      {result && (
        <p
          className={`mt-2 flex items-start gap-1.5 text-xs ${result.ok ? "text-muted" : "text-danger"}`}
        >
          {result.ok ? (
            <CircleCheck className="mt-px size-3.5 shrink-0 text-accent" />
          ) : (
            <CircleX className="mt-px size-3.5 shrink-0" />
          )}
          {result.ok
            ? `Connected. ${result.models.length} models available.`
            : result.error}
        </p>
      )}
      {test.error && (
        <p className="mt-2 text-xs text-danger">{errorMessage(test.error)}</p>
      )}
      {remove.error && (
        <p className="mt-2 text-xs text-danger">{errorMessage(remove.error)}</p>
      )}
      {editing && (
        <EditProvider
          provider={provider}
          kind={kind}
          onDone={() => setEditing(false)}
        />
      )}
    </li>
  );
}

function EditProvider({
  provider,
  kind,
  onDone,
}: {
  provider: ProviderView;
  kind: ProviderKindInfo | undefined;
  onDone: () => void;
}) {
  const queryClient = useQueryClient();
  const [name, setName] = useState(provider.name);
  const [baseUrl, setBaseUrl] = useState(provider.baseUrl ?? "");
  const [apiKey, setApiKey] = useState("");
  const save = useMutation({
    mutationFn: (body: Record<string, string>) =>
      apiJson<ProviderView>(`/api/settings/providers/${provider.id}`, {
        method: "PATCH",
        json: body,
      }),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: PROVIDERS });
      onDone();
    },
  });
  const submit = (event: SubmitEvent<HTMLFormElement>) => {
    event.preventDefault();
    const body: Record<string, string> = { name, baseUrl };
    if (apiKey) body.apiKey = apiKey;
    save.mutate(body);
  };
  return (
    <form
      onSubmit={submit}
      className="mt-3 grid gap-2 border-t border-border pt-3 sm:grid-cols-2"
    >
      <label className="text-xs text-muted">
        Name
        <input
          className={input}
          value={name}
          onChange={(e) => setName(e.target.value)}
        />
      </label>
      <label className="text-xs text-muted">
        Base URL
        <input
          className={input}
          value={baseUrl}
          placeholder={kind?.defaultBaseUrl ?? "https://…/v1"}
          onChange={(e) => setBaseUrl(e.target.value)}
        />
      </label>
      <label className="text-xs text-muted sm:col-span-2">
        API key {provider.hasApiKey && "(leave empty to keep the saved key)"}
        <input
          className={input}
          type="password"
          autoComplete="off"
          value={apiKey}
          onChange={(e) => setApiKey(e.target.value)}
        />
      </label>
      {save.error && (
        <p className="text-xs text-danger sm:col-span-2">
          {errorMessage(save.error)}
        </p>
      )}
      <div className="flex gap-2 sm:col-span-2">
        <Button type="submit" variant="primary" disabled={save.isPending}>
          Save
        </Button>
        {provider.hasApiKey && (
          <Button
            variant="danger"
            onClick={() => save.mutate({ apiKey: "" })}
            disabled={save.isPending}
          >
            <KeyRound className="size-4" /> Remove key
          </Button>
        )}
        <Button variant="ghost" onClick={onDone}>
          Cancel
        </Button>
      </div>
    </form>
  );
}

function AddProvider({
  kinds,
  scope,
}: {
  kinds: ProviderKindInfo[];
  scope: AiScope;
}) {
  const queryClient = useQueryClient();
  const [kind, setKind] = useState<ProviderKind>("openai");
  const [name, setName] = useState("");
  const [baseUrl, setBaseUrl] = useState("");
  const [apiKey, setApiKey] = useState("");
  const info = kinds.find((k) => k.kind === kind);
  const create = useMutation({
    mutationFn: () =>
      apiJson<ProviderView>("/api/settings/providers", {
        method: "POST",
        json: {
          kind,
          name: name.trim() || info?.label || kind,
          baseUrl: baseUrl.trim() || undefined,
          apiKey: apiKey.trim() || undefined,
          shared: scope === "shared" ? true : undefined,
        },
      }),
    onSuccess: () => {
      setName("");
      setBaseUrl("");
      setApiKey("");
      void queryClient.invalidateQueries({ queryKey: PROVIDERS });
    },
  });
  return (
    <form
      onSubmit={(event) => {
        event.preventDefault();
        create.mutate();
      }}
      className="grid gap-2 rounded-lg border border-dashed border-border p-4 sm:grid-cols-2"
    >
      <p className="text-sm font-medium sm:col-span-2">
        {scope === "shared"
          ? "Add a shared provider"
          : scope === "mine"
            ? "Add a personal provider"
            : "Add a provider"}
      </p>
      <label className="text-xs text-muted">
        Type
        <select
          className={input}
          value={kind}
          onChange={(e) => setKind(e.target.value as ProviderKind)}
        >
          {kinds.map((k) => (
            <option key={k.kind} value={k.kind}>
              {k.label}
            </option>
          ))}
        </select>
      </label>
      <label className="text-xs text-muted">
        Name
        <input
          className={input}
          value={name}
          placeholder={info?.label}
          onChange={(e) => setName(e.target.value)}
        />
      </label>
      <label className="text-xs text-muted">
        Base URL {info?.needsBaseUrl ? "(required)" : "(optional)"}
        <input
          className={input}
          value={baseUrl}
          placeholder={info?.defaultBaseUrl ?? "http://localhost:1234/v1"}
          onChange={(e) => setBaseUrl(e.target.value)}
        />
      </label>
      <label className="text-xs text-muted">
        API key {info?.needsApiKey ? "" : "(optional)"}
        <input
          className={input}
          type="password"
          autoComplete="off"
          value={apiKey}
          onChange={(e) => setApiKey(e.target.value)}
        />
      </label>
      {create.error && (
        <p className="text-xs text-danger sm:col-span-2">
          {errorMessage(create.error)}
        </p>
      )}
      <div className="sm:col-span-2">
        <Button type="submit" variant="primary" disabled={create.isPending}>
          <Plus className="size-4" /> Add provider
        </Button>
      </div>
    </form>
  );
}

function RolesForm({
  scope,
  providers,
  models,
  onModels,
}: {
  scope: AiScope;
  providers: ProviderView[];
  models: Record<string, string[]>;
  onModels: (
    update: (current: Record<string, string[]>) => Record<string, string[]>,
  ) => void;
}) {
  const queryClient = useQueryClient();
  // The person's own choices, or (admin page) the defaults everyone else falls back to.
  const path =
    scope === "shared" ? "/api/settings/default-roles" : "/api/settings/roles";
  const key = scope === "shared" ? DEFAULT_ROLES : ROLES;
  const roles = useQuery({
    queryKey: key,
    queryFn: () => apiJson<Roles>(path),
  });
  const defaults = useQuery({
    queryKey: DEFAULT_ROLES,
    queryFn: () => apiJson<Roles>("/api/settings/default-roles"),
    enabled: scope === "mine",
  });
  const [agent, setAgent] = useState<ModelRef | null>(null);
  const [fast, setFast] = useState<ModelRef | null>(null);
  useEffect(() => {
    if (roles.data) {
      setAgent(roles.data.agent);
      setFast(roles.data.fast);
    }
  }, [roles.data]);
  const save = useMutation({
    mutationFn: () =>
      apiJson<Roles>(path, {
        method: "PUT",
        json: { agent, fast },
      }),
    onSuccess: (saved) => {
      queryClient.setQueryData(key, saved);
      // The defaults also show on the Settings page.
      if (scope === "shared")
        void queryClient.invalidateQueries({ queryKey: ROLES });
    },
  });
  const describe = (model: ModelRef | null | undefined, none: string) =>
    model
      ? `${providers.find((p) => p.id === model.providerId)?.name ?? "a provider"} · ${model.model}`
      : none;
  const requested = useRef(new Set<string>());
  const load = useCallback(
    async (providerId: string) => {
      if (requested.current.has(providerId)) return;
      requested.current.add(providerId);
      try {
        const result = await apiJson<ProviderTestResult>(
          `/api/settings/providers/${providerId}/test`,
          { method: "POST" },
        );
        if (result.ok) {
          onModels((current) => ({ ...current, [providerId]: result.models }));
        }
      } catch {
        // The model name can still be typed.
      }
    },
    [onModels],
  );

  return (
    <section className="flex flex-col gap-4">
      <div>
        <h2 className="text-lg font-semibold">
          {scope === "shared"
            ? "Default models"
            : scope === "mine"
              ? "Your models"
              : "Models"}
        </h2>
        <p className="text-sm text-muted">
          The agent model answers the chat and proposes edits; it should support
          tool calling. The fast model runs quick actions on a selection.
          {scope === "shared" &&
            " Everyone who has not chosen their own models uses these."}
          {scope === "mine" &&
            " Leave a role empty to use the default an admin chose."}
        </p>
      </div>
      {providers.length === 0 ? (
        <p className="text-sm text-muted">
          {scope === "shared"
            ? "Add a shared provider first."
            : "Add a provider first."}
        </p>
      ) : (
        <>
          <RolePicker
            label="Agent"
            value={agent}
            providers={providers}
            models={models}
            onLoad={load}
            onChange={setAgent}
            emptyLabel={scope === "mine" ? "Use the default" : undefined}
            hint={
              scope === "mine"
                ? `Default: ${describe(defaults.data?.agent, "none yet; ask an admin, or choose one")}`
                : undefined
            }
          />
          <RolePicker
            label="Fast"
            value={fast}
            providers={providers}
            models={models}
            onLoad={load}
            onChange={setFast}
            optional
            emptyLabel={scope === "mine" ? "Use the default" : undefined}
            hint={
              scope === "mine"
                ? `Default: ${describe(defaults.data?.fast, "same as Agent")}`
                : undefined
            }
          />
          {save.error && (
            <p className="text-xs text-danger">{errorMessage(save.error)}</p>
          )}
          <div className="flex items-center gap-3">
            <Button
              variant="primary"
              onClick={() => save.mutate()}
              disabled={save.isPending}
            >
              Save models
            </Button>
            {save.isSuccess && (
              <span className="text-xs text-muted">Saved.</span>
            )}
          </div>
        </>
      )}
    </section>
  );
}

function RolePicker({
  label,
  value,
  providers,
  models,
  optional = false,
  emptyLabel,
  hint,
  onLoad,
  onChange,
}: {
  label: string;
  emptyLabel?: string;
  hint?: ReactNode;
  value: ModelRef | null;
  providers: ProviderView[];
  models: Record<string, string[]>;
  optional?: boolean;
  onLoad: (providerId: string) => void;
  onChange: (value: ModelRef | null) => void;
}) {
  const listId = `models-${label}`;
  const providerId = value?.providerId ?? "";
  const options = useMemo(() => models[providerId] ?? [], [models, providerId]);
  useEffect(() => {
    if (providerId) onLoad(providerId);
  }, [providerId, onLoad]);
  return (
    <div className="flex flex-col gap-1">
      <div className="grid items-end gap-2 sm:grid-cols-[6rem_1fr_1fr_7rem]">
        <span className="text-sm font-medium">{label}</span>
        <label className="text-xs text-muted">
          Provider
          <select
            className={input}
            value={providerId}
            onChange={(e) =>
              onChange(
                e.target.value
                  ? { providerId: e.target.value, model: "" }
                  : null,
              )
            }
          >
            <option value="">
              {emptyLabel ?? (optional ? "Same as agent" : "Choose…")}
            </option>
            {providers.map((p) => (
              <option key={p.id} value={p.id}>
                {p.name}
              </option>
            ))}
          </select>
        </label>
        <label className="text-xs text-muted">
          Model
          <input
            className={input}
            list={listId}
            value={value?.model ?? ""}
            disabled={!providerId}
            placeholder={
              options.length > 0 ? "Pick or type a model" : "Type a model name"
            }
            onChange={(e) =>
              providerId &&
              onChange({
                providerId,
                model: e.target.value,
                reasoning: value?.reasoning,
              })
            }
          />
          <datalist id={listId}>
            {options.map((model) => (
              <option key={model} value={model} />
            ))}
          </datalist>
        </label>
        <label
          className="text-xs text-muted"
          title="How much the model thinks before answering, for models that support it"
        >
          Reasoning
          <select
            className={input}
            value={value?.reasoning ?? ""}
            disabled={!providerId}
            onChange={(e) =>
              value &&
              onChange({ ...value, reasoning: parseReasoning(e.target.value) })
            }
          >
            {REASONING_OPTIONS.map((option) => (
              <option key={option.value} value={option.value}>
                {option.label}
              </option>
            ))}
          </select>
        </label>
      </div>
      {hint && <p className="text-xs text-muted sm:pl-[6.5rem]">{hint}</p>}
    </div>
  );
}
