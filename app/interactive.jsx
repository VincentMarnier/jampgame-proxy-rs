"use client";

import { useId, useMemo, useRef, useState } from "react";

async function copyText(text) {
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch {
    try {
      const ta = document.createElement("textarea");
      ta.value = text;
      ta.setAttribute("readonly", "");
      ta.style.position = "absolute";
      ta.style.left = "-9999px";
      document.body.appendChild(ta);
      ta.select();
      document.execCommand("copy");
      document.body.removeChild(ta);
      return true;
    } catch {
      return false;
    }
  }
}

export function CopyButton({ text, label = "Copy" }) {
  const [done, setDone] = useState(false);
  const timer = useRef(null);
  return (
    <button
      type="button"
      className={`copy-btn${done ? " copied" : ""}`}
      onClick={async () => {
        const ok = await copyText(text);
        if (!ok) return;
        setDone(true);
        clearTimeout(timer.current);
        timer.current = setTimeout(() => setDone(false), 1600);
      }}
      aria-live="polite"
    >
      {done ? "Copied ✓" : label}
    </button>
  );
}

export function CmdChip({ cmd }) {
  const [done, setDone] = useState(false);
  const timer = useRef(null);
  return (
    <button
      type="button"
      className={`cmd${done ? " copied" : ""}`}
      title={`Copy: ${cmd}`}
      aria-label={`Copy command ${cmd}`}
      onClick={async () => {
        const ok = await copyText(cmd);
        if (!ok) return;
        setDone(true);
        clearTimeout(timer.current);
        timer.current = setTimeout(() => setDone(false), 1600);
      }}
    >
      {done ? `${cmd} ✓` : cmd}
    </button>
  );
}

export function CvarExplorer({ rows }) {
  const [query, setQuery] = useState("");
  const inputId = useId();
  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!q) return rows;
    return rows.filter(
      ([name, def, desc, tag]) =>
        name.toLowerCase().includes(q) ||
        desc.toLowerCase().includes(q) ||
        def.toLowerCase().includes(q) ||
        (tag && tag.toLowerCase().includes(q))
    );
  }, [query, rows]);

  return (
    <div className="explorer">
      <div className="explorer-bar">
        <div className="search-wrap">
          <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" aria-hidden="true">
            <circle cx="11" cy="11" r="7" />
            <path d="m20 20-3.5-3.5" />
          </svg>
          <label htmlFor={inputId} className="sr-only" style={{ position: "absolute", left: "-9999px" }}>
            Filter server cvars
          </label>
          <input
            id={inputId}
            type="search"
            placeholder="Filter cvars — try “tffa”, “team”, “flood”…"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            autoComplete="off"
          />
        </div>
        <span className="result-count" role="status">
          {filtered.length} / {rows.length} cvars
        </span>
      </div>
      <div className="table-wrap">
        {filtered.length > 0 ? (
          <table>
            <caption style={{ position: "absolute", left: "-9999px" }}>
              Server cvars provided by the proxy
            </caption>
            <thead>
              <tr>
                <th scope="col">cvar</th>
                <th scope="col">default</th>
                <th scope="col">description</th>
              </tr>
            </thead>
            <tbody>
              {filtered.map(([name, def, desc, tag]) => (
                <tr key={name}>
                  <td>
                    <code>{name}</code>
                    {tag ? (
                      <>
                        <br />
                        <span className="tag">{tag}</span>
                      </>
                    ) : null}
                  </td>
                  <td>
                    <code>{def}</code>
                  </td>
                  <td>{desc}</td>
                </tr>
              ))}
            </tbody>
          </table>
        ) : (
          <p className="empty-state">
            No cvar matches “{query}”. Try “tffa”, “team” or “net”.
          </p>
        )}
      </div>
    </div>
  );
}
