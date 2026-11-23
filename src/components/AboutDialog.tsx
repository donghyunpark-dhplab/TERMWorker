import { useEffect, useState } from "react";
import { getVersion } from "@tauri-apps/api/app";

import { Modal } from "./Modal";

export const COMPANY = "DHPLab Consulting";
export const WEBSITE = "dhplab.com";
export const SUPPORT_EMAIL = "donghyun.park@dhplab.com";

const FEATURES: [string, string][] = [
  ["Serial, SSH, Telnet, raw TCP", "Every console you use, in tabs. Saved sessions and one-line quick connect."],
  ["Built for USB-serial", "Reconnects by itself when an adapter is unplugged and plugged back in. Break, DTR/RTS and flow control."],
  ["X/Y/ZMODEM", "Send and receive over any connection, U-Boot included. ZMODEM starts on its own when you run sz or rz."],
  ["HDLC frames", "RFC 1662 framing with FCS-16/32. Frames go to a side panel while console text stays in the terminal."],
  ["Logs you can read", "Raw, plain text, or text with a millisecond timestamp on every line."],
  ["Korean and CJK", "Wide characters drawn correctly. UTF-8, CP949, GB18030, Big5 and more, switchable per tab."],
];

export function AboutDialog({ onClose }: { onClose: () => void }) {
  const [version, setVersion] = useState<string>("");

  useEffect(() => {
    getVersion()
      .then(setVersion)
      .catch(() => setVersion(""));
  }, []);

  return (
    <Modal title="About TermWorker" width={560} onClose={onClose}>
      <div className="about">
        <div className="about-head">
          <img src="/logo.svg" alt="" width={72} height={72} />
          <div>
            <h3>
              TermWorker {version && <span className="about-version">{version}</span>}
            </h3>
            <p className="about-tagline">The terminal for people who bring up boards, modems and routers.</p>
          </div>
        </div>

        <ul className="about-features">
          {FEATURES.map(([title, text]) => (
            <li key={title}>
              <strong>{title}</strong>
              <span>{text}</span>
            </li>
          ))}
        </ul>

        <p className="about-note">
          Windows and Linux. Passwords and passphrases are never written to disk. File transfer and HDLC are
          implemented in-house.
        </p>

        <div className="about-company">
          <img src="/dhplab-logo-dark.svg" alt={COMPANY} width={154} height={44} />
          <div className="about-meta">
            <div>© 2026 {COMPANY}. Licensed under MIT or Apache-2.0.</div>
            <div className="selectable">
              {WEBSITE} · {SUPPORT_EMAIL}
            </div>
          </div>
        </div>
      </div>
    </Modal>
  );
}
