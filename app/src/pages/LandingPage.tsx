import { Link } from 'react-router';

// Minimal placeholder landing page: just a way into the terminal.
// Replaces an earlier, more built-out version that didn't land — kept
// deliberately bare until there's real direction to build from.

export function LandingPage() {
  return (
    <div className="flex min-h-screen w-full items-center justify-center bg-surface-base">
      <Link
        to="/trade"
        className="rounded-md bg-accent px-[var(--space-8)] py-[var(--space-5)] text-lg font-semibold text-white transition-colors duration-150 hover:bg-accent-strong"
      >
        Launch Terminal
      </Link>
    </div>
  );
}
