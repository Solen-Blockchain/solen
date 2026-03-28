export const metadata = {
  title: "Solen Explorer",
  description: "Block explorer for the Solen network",
};

export default function RootLayout({ children }: { children: React.ReactNode }) {
  return (
    <html lang="en">
      <body>{children}</body>
    </html>
  );
}
