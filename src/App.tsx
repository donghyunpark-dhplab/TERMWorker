import { useCallback, useEffect, useRef, useState } from "react";

import { AboutDialog } from "./components/AboutDialog";
import { ConnectDialog } from "./components/ConnectDialog";
import { SessionPrompts } from "./components/PromptDialogs";
import { ScriptDialogs, ScriptPanel } from "./components/Script";
import { Sidebar } from "./components/Sidebar";
import { StatusBar } from "./components/StatusBar";
import { TabBar } from "./components/TabBar";
import { TftpPanel } from "./components/TftpPanel";
import { TerminalPane } from "./components/TerminalPane";
import { Welcome } from "./components/Welcome";
import { api } from "./lib/api";
import { scripts } from "./lib/script";
import { defaultConnection, defaultOptions, newId, normalizeProfile, profileFromConnection } from "./lib/profiles";
import type { Connection, ConnectionKind, Profile } from "./lib/types";
import { TermSession } from "./terminal/TermSession";

interface DialogState {
  mode: "new" | "edit";
  initial: Profile;
}

export function App() {
  const [tabs, setTabs] = useState<TermSession[]>([]);
  const [activeId, setActiveId] = useState<string | null>(null);
  const [profiles, setProfiles] = useState<Profile[]>([]);
  const [dialog, setDialog] = useState<DialogState | null>(null);
  const [findOpen, setFindOpen] = useState(false);
  const [sidebarOpen, setSidebarOpen] = useState(true);
  const [aboutOpen, setAboutOpen] = useState(false);

  // Per-tab UI state (status, prompts) is rendered by components that subscribe with
  // useSyncExternalStore; App itself only re-renders when tabs are added or removed.
  const active = tabs.find((t) => t.tabId === activeId) ?? null;

  // ---- Saved sessions ------------------------------------------------------

  useEffect(() => {
    api
      .profilesLoad()
      .then((f) => setProfiles((f.sessions ?? []).map(normalizeProfile)))
      .catch((e) => console.error("Could not load sessions", e));
  }, []);

  const persist = useCallback((next: Profile[]) => {
    setProfiles(next);
    api.profilesSave({ version: 1, sessions: next }).catch((e) => console.error("Could not save sessions", e));
  }, []);

  const upsertProfile = useCallback(
    (p: Profile) => {
      const exists = profiles.some((x) => x.id === p.id);
      persist(exists ? profiles.map((x) => (x.id === p.id ? p : x)) : [...profiles, p]);
    },
    [profiles, persist],
  );

  // ---- Tabs ----------------------------------------------------------------

  const openTab = useCallback((profile: Profile, secret: string | null = null, tabId?: string) => {
    const s = new TermSession(profile, secret, tabId);
    setTabs((t) => [...t, s]);
    setActiveId(s.tabId);
    return s;
  }, []);

  const closeTab = useCallback(
    (id: string) => {
      const idx = tabs.findIndex((t) => t.tabId === id);
      if (idx < 0) return;
      tabs[idx].dispose();
      const rest = tabs.filter((t) => t.tabId !== id);
      setTabs(rest);
      if (activeId === id) setActiveId(rest[Math.min(idx, rest.length - 1)]?.tabId ?? null);
    },
    [tabs, activeId],
  );

  const cycleTab = useCallback(
    (delta: number) => {
      if (tabs.length < 2) return;
      const idx = tabs.findIndex((t) => t.tabId === activeId);
      setActiveId(tabs[(idx + delta + tabs.length) % tabs.length].tabId);
    },
    [tabs, activeId],
  );

  const newSession = useCallback((kind: ConnectionKind = "serial") => {
    setDialog({
      mode: "new",
      initial: { id: newId(), name: "", connection: defaultConnection(kind), options: defaultOptions() },
    });
  }, []);

  const quickConnect = useCallback(
    (c: Connection) => {
      if (c.kind === "ssh" && !c.username) {
        // Need a user name: let the dialog collect it.
        setDialog({ mode: "new", initial: profileFromConnection(c) });
        return;
      }
      openTab(profileFromConnection(c));
    },
    [openTab],
  );

  // ---- Scripts ---------------------------------------------------------------

  // Scripts ask for tabs and profiles at any time: hand them the latest.
  const latest = useRef({ tabs, profiles, closeTab });
  latest.current = { tabs, profiles, closeTab };
  useEffect(() => {
    scripts.init({
      tabs: () => latest.current.tabs,
      profiles: () => latest.current.profiles,
      openTab: (p, secret, tabId) => {
        const t = openTab(p, secret, tabId);
        // A script may ask about the tab before React has re-rendered.
        latest.current.tabs = [...latest.current.tabs, t];
        return t;
      },
      closeTab: (id) => latest.current.closeTab(id),
      activate: (id) => setActiveId(id),
    });
  }, [openTab]);

  // ---- Keyboard shortcuts --------------------------------------------------

  const handlers = useRef({ newSession, closeTab, cycleTab, active, dialog });
  handlers.current = { newSession, closeTab, cycleTab, active, dialog };

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const h = handlers.current;
      if (h.dialog) return;
      const key = e.key.toLowerCase();
      let handled = true;
      if (e.ctrlKey && e.shiftKey && key === "t") h.newSession();
      else if (e.ctrlKey && e.shiftKey && key === "w") h.active && h.closeTab(h.active.tabId);
      else if (e.ctrlKey && key === "tab") h.cycleTab(e.shiftKey ? -1 : 1);
      else if (e.ctrlKey && !e.shiftKey && key === "pagedown") h.cycleTab(1);
      else if (e.ctrlKey && !e.shiftKey && key === "pageup") h.cycleTab(-1);
      else if (e.ctrlKey && e.shiftKey && key === "f") setFindOpen(true);
      else if (e.ctrlKey && e.shiftKey && key === "c") h.active?.copySelection();
      else if (e.ctrlKey && e.shiftKey && key === "v") void h.active?.paste();
      else if (e.ctrlKey && e.shiftKey && key === "b") setSidebarOpen((v) => !v);
      else if (e.ctrlKey && !e.shiftKey && key === "k") {
        setSidebarOpen(true);
        requestAnimationFrame(() => document.getElementById("quick-connect")?.focus());
      } else handled = false;
      if (handled) {
        e.preventDefault();
        e.stopPropagation();
      }
    };
    // Capture phase: runs before xterm.js sees the key.
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, []);

  // Suppress the browser context menu outside editable fields.
  useEffect(() => {
    const onMenu = (e: MouseEvent) => {
      const el = e.target as HTMLElement;
      if (!el.closest("input, textarea")) e.preventDefault();
    };
    window.addEventListener("contextmenu", onMenu);
    return () => window.removeEventListener("contextmenu", onMenu);
  }, []);

  useEffect(() => setFindOpen(false), [activeId]);

  // ---- Render --------------------------------------------------------------

  return (
    <div className={`app ${sidebarOpen ? "" : "no-sidebar"}`}>
      {sidebarOpen && (
        <Sidebar
          profiles={profiles}
          onOpen={(p) => openTab(p)}
          onEdit={(p) => setDialog({ mode: "edit", initial: p })}
          onDuplicate={(p) => upsertProfile({ ...structuredClone(p), id: newId(), name: `${p.name} (copy)` })}
          onDelete={(p) => persist(profiles.filter((x) => x.id !== p.id))}
          onNew={() => newSession()}
          onQuickConnect={quickConnect}
          onAbout={() => setAboutOpen(true)}
        />
      )}

      <main className="main">
        <TabBar
          tabs={tabs}
          activeId={activeId}
          onSelect={setActiveId}
          onClose={closeTab}
          onNew={() => newSession()}
          onToggleSidebar={() => setSidebarOpen((v) => !v)}
        />
        <div className="terminal-area">
          {tabs.map((t) => (
            <TerminalPane
              key={t.tabId}
              session={t}
              active={t.tabId === activeId}
              findOpen={findOpen}
              onCloseFind={() => setFindOpen(false)}
            />
          ))}
          {tabs.length === 0 && <Welcome profiles={profiles} onNew={newSession} onOpen={(p) => openTab(p)} onAbout={() => setAboutOpen(true)} />}
        </div>
        <TftpPanel active={active} />
        <ScriptPanel />
        <StatusBar session={active} />
      </main>

      {aboutOpen && <AboutDialog onClose={() => setAboutOpen(false)} />}

      {dialog && (
        <ConnectDialog
          initial={dialog.initial}
          mode={dialog.mode}
          onCancel={() => setDialog(null)}
          onSave={(p) => {
            upsertProfile(p);
            setDialog(null);
          }}
          onConnect={(p, secret, save) => {
            if (save) upsertProfile(p);
            setDialog(null);
            openTab(p, secret);
          }}
        />
      )}
      {!dialog && <SessionPrompts session={active} />}
      {!dialog && <ScriptDialogs />}
    </div>
  );
}
