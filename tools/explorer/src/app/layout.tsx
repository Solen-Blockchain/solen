export const metadata = {
  title: "Solen Explorer",
  description: "Block explorer for the Solen network",
};

export default function RootLayout({ children }: { children: React.ReactNode }) {
  return (
    <html lang="en">
      <body style={{ margin: 0, background: "#fff", color: "#111" }}>
        {children}
      </body>
    </html>
  );
}
