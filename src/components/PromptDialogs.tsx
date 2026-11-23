import { useEffect, useState, useSyncExternalStore } from "react";

import { downloadDir, pickFilesToSend, rememberDownloadDir } from "../lib/transfer";
import type { TermSession } from "../terminal/TermSession";
import { open } from "@tauri-apps/plugin-dialog";
import { IconFolder, IconKey, IconShield, IconTransfer } from "./Icons";
import { Modal } from "./Modal";

/**
 * Shows the host-key or password prompt of the active tab.
 *
 * Uses useSyncExternalStore, which re-checks the snapshot right after subscribing.
 * A tab can raise its password prompt synchronously while mounting, before any
 * plain useEffect subscription exists, so a manual subscription would miss it.
 */
export function SessionPrompts({ session }: { session: TermSession | null }) {
  return session ? <ActivePrompts key={session.tabId} s={session} /> : null;
}

function ActivePrompts({ s }: { s: TermSession }) {
  useSyncExternalStore(s.subscribe, s.getSnapshot);
  if (s.hostKey) return <HostKeyDialog session={s} />;
  if (s.secretPrompt) return <SecretDialog key={s.secretPrompt.error ?? ""} session={s} />;
  if (s.zmodemRequest) return <ZmodemDialog session={s} />;
  return null;
}

export function SecretDialog({ session }: { session: TermSession }) {
  const prompt = session.secretPrompt!;
  const [value, setValue] = useState("");
  const c = session.profile.connection;
  const who = c.kind === "ssh" ? `${c.username}@${c.host}` : session.title;
  const isPass = prompt.kind === "passphrase";

  return (
    <Modal
      title={isPass ? "Key passphrase" : "Password"}
      icon={<IconKey />}
      width={420}
      onClose={() => session.cancelSecret()}
      footer={
        <>
          <button className="btn" onClick={() => session.cancelSecret()}>
            Cancel
          </button>
          <button className="btn primary" form="secret-form" type="submit">
            Connect
          </button>
        </>
      }
    >
      <form
        id="secret-form"
        onSubmit={(e) => {
          e.preventDefault();
          session.provideSecret(value);
        }}
      >
        <p className="muted">
          {isPass ? "Enter the passphrase for the private key used by " : "Enter the password for "}
          <strong>{who}</strong>.
        </p>
        {prompt.error && <p className="form-error">{prompt.error}</p>}
        <input
          className="input"
          type="password"
          autoFocus
          value={value}
          placeholder={isPass ? "Passphrase (leave empty if none)" : "Password"}
          onChange={(e) => setValue(e.target.value)}
        />
        <p className="hint">Not stored. You will be asked again after restarting TermWorker.</p>
      </form>
    </Modal>
  );
}

export function HostKeyDialog({ session }: { session: TermSession }) {
  const hk = session.hostKey!;
  return (
    <Modal
      title="Unknown host key"
      icon={<IconShield />}
      width={540}
      onClose={() => session.answerHostKey("reject")}
      footer={
        <>
          <button className="btn" onClick={() => session.answerHostKey("reject")}>
            Cancel
          </button>
          <button className="btn" onClick={() => session.answerHostKey("acceptOnce")}>
            Connect once
          </button>
          <button className="btn primary" autoFocus onClick={() => session.answerHostKey("acceptAndSave")}>
            Trust &amp; connect
          </button>
        </>
      }
    >
      <p>
        TermWorker has not connected to{" "}
        <strong>
          {hk.host}:{hk.port}
        </strong>{" "}
        before and cannot verify its identity.
      </p>
      <div className="fingerprint">
        <div className="fp-label">{hk.algorithm} key fingerprint</div>
        <code>{hk.fingerprint}</code>
      </div>
      <p className="muted">
        Compare this fingerprint with the one shown by the server administrator (for example with{" "}
        <code>ssh-keygen -lf /etc/ssh/ssh_host_ed25519_key.pub</code>). Choose <em>Trust</em> to remember the key; you
        will be warned if it ever changes.
      </p>
    </Modal>
  );
}

/** The remote side ran `sz` or `rz`: ask what to do. */
export function ZmodemDialog({ session }: { session: TermSession }) {
  const remoteSends = session.zmodemRequest!.remoteSends;
  const [dir, setDir] = useState<string | null>(null);
  // A native file dialog is open. On Linux it does not block the window, so a
  // second click would open another one.
  const [picking, setPicking] = useState(false);

  useEffect(() => {
    if (remoteSends) downloadDir().then(setDir, () => setDir(""));
  }, [remoteSends]);

  const chooseDir = async () => {
    if (picking) return;
    setPicking(true);
    try {
      const picked = await open({ title: "Save received files in…", directory: true, defaultPath: dir ?? undefined });
      if (typeof picked === "string") {
        setDir(picked);
        rememberDownloadDir(picked); // an explicit choice, kept even if this upload is declined
      }
    } finally {
      setPicking(false);
    }
  };

  const accept = async () => {
    if (remoteSends) {
      if (!dir) return;
      rememberDownloadDir(dir);
      session.acceptZmodem(dir);
    } else {
      if (picking) return;
      setPicking(true);
      try {
        const files = await pickFilesToSend("zmodem");
        if (files?.length) session.acceptZmodem(files);
        else session.declineZmodem();
      } finally {
        setPicking(false);
      }
    }
  };

  return (
    <Modal
      title={remoteSends ? "Receive files" : "Send files"}
      icon={<IconTransfer />}
      width={480}
      onClose={() => session.declineZmodem()}
      footer={
        <>
          <button className="btn" onClick={() => session.declineZmodem()}>
            Decline
          </button>
          <button
            className="btn primary"
            autoFocus
            disabled={picking || (remoteSends && !dir)}
            onClick={() => void accept()}
          >
            {remoteSends ? "Receive" : "Choose files…"}
          </button>
        </>
      }
    >
      {remoteSends ? (
        <>
          <p>
            The remote side started a ZMODEM upload (<code>sz</code>).
          </p>
          <label className="field-label">Save to</label>
          <div className="row">
            <input className="input grow mono" readOnly value={dir ?? ""} placeholder="Loading…" />
            <button
              className="icon-btn bordered"
              title="Choose folder"
              disabled={picking}
              onClick={() => void chooseDir()}
            >
              <IconFolder />
            </button>
          </div>
          <p className="hint">Existing files are kept; a new copy gets a number, e.g. "file (1).bin".</p>
        </>
      ) : (
        <p>
          The remote side is waiting for files over ZMODEM (<code>rz</code>). Choose the files to send.
        </p>
      )}
    </Modal>
  );
}
