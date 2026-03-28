"use client";

import { useEffect, useState } from "react";

interface ChainStatus {
  latest_height: number;
  total_blocks: number;
  total_txs: number;
  total_events: number;
}

interface Block {
  height: number;
  epoch: number;
  state_root: string;
  proposer: string;
  timestamp_ms: number;
  tx_count: number;
  gas_used: number;
}

const API = process.env.NEXT_PUBLIC_API_URL || "http://127.0.0.1:9955";

function truncate(s: string, n: number = 12): string {
  if (s.length <= n) return s;
  return s.slice(0, n) + "...";
}

function timeAgo(ms: number): string {
  const seconds = Math.floor((Date.now() - ms) / 1000);
  if (seconds < 60) return `${seconds}s ago`;
  if (seconds < 3600) return `${Math.floor(seconds / 60)}m ago`;
  return `${Math.floor(seconds / 3600)}h ago`;
}

export default function Home() {
  const [status, setStatus] = useState<ChainStatus | null>(null);
  const [blocks, setBlocks] = useState<Block[]>([]);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    const fetchData = async () => {
      try {
        const [statusRes, blocksRes] = await Promise.all([
          fetch(`${API}/api/status`),
          fetch(`${API}/api/blocks?limit=15`),
        ]);
        setStatus(await statusRes.json());
        setBlocks(await blocksRes.json());
        setError(null);
      } catch (e) {
        setError("Cannot connect to explorer API. Is the node running?");
      }
    };

    fetchData();
    const interval = setInterval(fetchData, 2000);
    return () => clearInterval(interval);
  }, []);

  return (
    <main style={{ maxWidth: 960, margin: "0 auto", padding: 24, fontFamily: "system-ui" }}>
      <h1 style={{ fontSize: 28, marginBottom: 8 }}>Solen Explorer</h1>
      <p style={{ color: "#666", marginBottom: 24 }}>Block explorer for the Solen network</p>

      {error && (
        <div style={{ background: "#fff3cd", padding: 12, borderRadius: 6, marginBottom: 16 }}>
          {error}
        </div>
      )}

      {status && (
        <div style={{ display: "grid", gridTemplateColumns: "repeat(4, 1fr)", gap: 12, marginBottom: 32 }}>
          <StatCard label="Height" value={status.latest_height.toLocaleString()} />
          <StatCard label="Blocks" value={status.total_blocks.toLocaleString()} />
          <StatCard label="Transactions" value={status.total_txs.toLocaleString()} />
          <StatCard label="Events" value={status.total_events.toLocaleString()} />
        </div>
      )}

      <h2 style={{ fontSize: 20, marginBottom: 12 }}>Recent Blocks</h2>
      <table style={{ width: "100%", borderCollapse: "collapse", fontSize: 14 }}>
        <thead>
          <tr style={{ borderBottom: "2px solid #e5e7eb", textAlign: "left" }}>
            <th style={{ padding: "8px 4px" }}>Height</th>
            <th style={{ padding: "8px 4px" }}>Epoch</th>
            <th style={{ padding: "8px 4px" }}>Txs</th>
            <th style={{ padding: "8px 4px" }}>Gas</th>
            <th style={{ padding: "8px 4px" }}>Proposer</th>
            <th style={{ padding: "8px 4px" }}>Time</th>
          </tr>
        </thead>
        <tbody>
          {blocks.map((block) => (
            <tr key={block.height} style={{ borderBottom: "1px solid #f3f4f6" }}>
              <td style={{ padding: "8px 4px", fontWeight: 600 }}>{block.height}</td>
              <td style={{ padding: "8px 4px" }}>{block.epoch}</td>
              <td style={{ padding: "8px 4px" }}>{block.tx_count}</td>
              <td style={{ padding: "8px 4px" }}>{block.gas_used.toLocaleString()}</td>
              <td style={{ padding: "8px 4px", fontFamily: "monospace", fontSize: 12 }}>
                {truncate(block.proposer)}
              </td>
              <td style={{ padding: "8px 4px", color: "#666" }}>
                {timeAgo(block.timestamp_ms)}
              </td>
            </tr>
          ))}
        </tbody>
      </table>

      {blocks.length === 0 && !error && (
        <p style={{ textAlign: "center", color: "#999", padding: 32 }}>
          No blocks yet. Start the node to begin producing blocks.
        </p>
      )}
    </main>
  );
}

function StatCard({ label, value }: { label: string; value: string }) {
  return (
    <div style={{
      background: "#f9fafb",
      border: "1px solid #e5e7eb",
      borderRadius: 8,
      padding: 16,
      textAlign: "center",
    }}>
      <div style={{ fontSize: 24, fontWeight: 700 }}>{value}</div>
      <div style={{ fontSize: 13, color: "#666", marginTop: 4 }}>{label}</div>
    </div>
  );
}
