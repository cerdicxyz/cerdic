import { useEffect, useRef } from 'react';
import { Link } from 'react-router';

// Natural size the live /trade iframe renders at before being scaled
// down to fit the laptop-frame's inset area — matches the desktop
// viewport this app's screenshots/testing have used all along, not an
// arbitrary number.
const DEMO_WIDTH = 1600;
const DEMO_HEIGHT = 1000;

export function LandingPage() {
  const demoContainerRef = useRef<HTMLDivElement>(null);
  const demoIframeRef = useRef<HTMLIFrameElement>(null);

  useEffect(() => {
    const container = demoContainerRef.current;
    const iframe = demoIframeRef.current;
    if (!container || !iframe) return;

    function resize() {
      if (!container || !iframe) return;
      const scale = container.clientWidth / DEMO_WIDTH;
      iframe.style.transform = `scale(${scale})`;
      iframe.style.height = `${DEMO_HEIGHT * scale}px`;
    }

    resize();
    const observer = new ResizeObserver(resize);
    observer.observe(container);
    return () => observer.disconnect();
  }, []);

  return (
    <div className="min-h-screen w-full bg-[#f4f4f7] text-[#0c0c14]">
      {/* Nav */}
      <header className="flex items-center justify-between gap-[var(--space-6)] px-[var(--space-9)] py-[var(--space-7)] sm:px-[32px]">
        <Link
          to="/"
          className="flex flex-shrink-0 items-center gap-[var(--space-4)] no-underline"
        >
          <img
            src="/logos/android-chrome-512x512.png"
            alt="Cerdic"
            className="h-8 w-8 rounded-lg object-cover"
          />
          <span className="text-[15px] font-medium tracking-tight text-[#0c0c14]">
            Cerdic
          </span>
        </Link>

        <nav className="hidden items-center gap-[2px] rounded-full border border-black/[0.06] bg-black/[0.03] p-[4px] md:flex">
          <a
            href="#markets"
            className="rounded-full bg-white px-[var(--space-6)] py-[var(--space-3)] text-[13px] font-medium text-[#0c0c14] no-underline shadow-[0_1px_2px_rgba(0,0,0,0.06)]"
          >
            Markets
          </a>
          <Link
            to="/trade"
            className="rounded-full px-[var(--space-6)] py-[var(--space-3)] text-[13px] font-normal text-black/55 no-underline transition-colors duration-150 hover:text-black/80"
          >
            Trade
          </Link>
          <Link
            to="/agents"
            className="rounded-full px-[var(--space-6)] py-[var(--space-3)] text-[13px] font-normal text-black/55 no-underline transition-colors duration-150 hover:text-black/80"
          >
            Agents
          </Link>
        </nav>

        <div className="flex flex-shrink-0 items-center gap-[var(--space-6)]">
          <Link
            to="/trade"
            className="hidden px-[var(--space-4)] py-[var(--space-3)] text-[13px] font-normal text-black/55 no-underline transition-colors duration-150 hover:text-black/80 sm:inline-block"
          >
            Log in
          </Link>
          <Link
            to="/trade"
            className="inline-flex items-center gap-[var(--space-3)] rounded-full bg-accent px-[var(--space-6)] py-[var(--space-3)] text-[13px] font-medium text-white no-underline transition-colors duration-150 hover:bg-accent-strong"
          >
            Sign up
            <svg width="12" height="12" viewBox="0 0 12 12" fill="none">
              <path
                d="M3 9L9 3M9 3H4M9 3V8"
                stroke="currentColor"
                strokeWidth="1.5"
                strokeLinecap="round"
                strokeLinejoin="round"
              />
            </svg>
          </Link>
        </div>
      </header>

      {/* Hero */}
      <section className="flex flex-col items-center px-[var(--space-9)] pb-[64px] pt-[var(--space-9)] text-center sm:px-[48px]">
        <div className="mx-auto mb-[var(--space-7)] inline-flex items-center gap-[var(--space-5)] rounded-full border border-[color-mix(in_oklch,var(--color-long)_25%,transparent)] bg-[color-mix(in_oklch,var(--color-long)_10%,transparent)] px-[var(--space-6)] py-[var(--space-3)]">
          <span className="inline-block h-[6px] w-[6px] flex-shrink-0 animate-pulse rounded-full bg-[var(--color-long)]" />
          <span className="text-[11px] font-normal uppercase tracking-[0.07em] text-[var(--color-long)]">
            Live on arc
          </span>
        </div>

        <h1 className="m-0 mb-[var(--space-7)] max-w-[720px] text-[clamp(28px,4.5vw,52px)] font-light leading-[1.15] tracking-[-0.01em] text-[#0c0c14]">
          Sealed order flow,
          <br /> settled on-chain
        </h1>

        <p className="m-0 mb-[var(--space-9)] max-w-[520px] text-[15px] font-light leading-relaxed text-black/55">
          Perpetuals across FX, crypto, and commodities, matched in a trusted
          execution environment and settled on-chain. Up to 50× leverage across
          9 markets.
        </p>

        <Link
          to="/trade"
          className="mx-auto inline-flex items-center gap-[var(--space-5)] rounded-full bg-accent px-[var(--space-9)] py-[var(--space-6)] text-[15px] font-normal text-white no-underline transition-colors duration-150 hover:bg-accent-strong"
        >
          Start trading
        </Link>
      </section>

      {/* Laptop-framed live demo */}
      <section className="w-full px-[var(--space-8)] pb-[80px] sm:px-[20px]">
        <div className="relative mx-auto max-w-[1320px]">
          {/* The frame image itself has no real transparency (checked: a
              flat, fully-opaque mockup screenshot, not a cutout template)
              — used here as a background/border layer, with the actual
              live app inset on top so its purple glow edge shows around
              the demo like a real laptop bezel. */}
          <div
            className="relative w-full overflow-hidden rounded-[18px]"
            style={{
              aspectRatio: '1428 / 642',
              backgroundImage: 'url(/laptop-frame.png)',
              backgroundSize: '100% 100%',
              boxShadow: '0 40px 100px rgba(0,0,0,0.45)',
            }}
          >
            {/* Live, real /trade app — not a static screenshot — scaled
                down via CSS transform from its natural desktop size to
                fit the inset screen area, same page a visitor lands on
                after "Start trading" above. */}
            <div
              ref={demoContainerRef}
              className="absolute overflow-hidden rounded-[8px] bg-[#0c0c0f]"
              style={{ inset: '2.2% 1.4%' }}
            >
              <iframe
                ref={demoIframeRef}
                src="/trade"
                title="Cerdic live trading demo"
                className="pointer-events-none origin-top-left border-0"
                style={{ width: `${DEMO_WIDTH}px`, height: `${DEMO_HEIGHT}px` }}
              />
            </div>
          </div>
        </div>
      </section>
    </div>
  );
}
