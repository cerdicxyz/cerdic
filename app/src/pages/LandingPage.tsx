import { useEffect, useRef, useState } from 'react';
import { Link } from 'react-router';

const FAQ_ITEMS: Array<{ q: string; a: string }> = [
  {
    q: 'What is Cerdic?',
    a: 'A perpetuals venue where order matching happens inside a trusted execution environment (TEE) ;your position size, entry price, and direction stay sealed off-chain, while settlement still happens on-chain, verifiably.',
  },
  {
    q: 'What can I trade on Cerdic?',
    a: 'FX majors, crypto, and commodities ;EURC/USDC, GBP/USD, AUD/USD, USD/JPY, BTC/USDC, HYPE/USD, XAU/USD, BRENT/USD, and KR200/USD, with up to 50x leverage depending on the market.',
  },
  {
    q: 'Why does order matching happen in a TEE?',
    a: "So the market can't see your position while it's open. A plaintext order book leaks your size and direction to anyone watching; sealing that inside a TEE means only the settlement outcome is public, not the position that produced it.",
  },
  {
    q: 'How much leverage can I use?',
    a: 'Up to 50x on FX majors, up to 30x on crypto and commodities ;each market has its own ceiling, shown in the market selector before you open a position.',
  },
  {
    q: 'How does settlement work if positions are sealed?',
    a: 'The TEE computes the real collateral delta off-chain and the settlement contract trusts that computation, attested by the enclave itself ;the contract never sees your plaintext size or price, only the resulting balance change.',
  },
  {
    q: 'Can agents trade on Cerdic?',
    a: 'Yes ;agent accounts are a first-class part of the system, not a bolt-on. An agent can hold its own sealed positions and settle through the same TEE-matched pipeline a human trader uses.',
  },
  {
    q: 'How do I start trading?',
    a: 'Connect a wallet, deposit collateral, and open a position from the trade ticket ;market or limit order, pick your leverage, and the fill settles on-chain once the TEE matches it.',
  },
  {
    q: 'What happens if my position gets liquidated?',
    a: 'Liquidation is computed the same way settlement is ;inside the TEE, using your sealed position and the live oracle price ;and the resulting collateral change is what actually posts on-chain.',
  },
];

/** Backspaces the current word down to nothing, then types the next one
 *  in `words`, looping — the "backspace Sealed, write Private" effect. */
function useCyclingWord(
  words: string[],
  typeMs = 65,
  deleteMs = 40,
  pauseMs = 1800,
  startDelayMs = 0,
) {
  const [wordIndex, setWordIndex] = useState(0);
  const [text, setText] = useState(words[0]);
  const [phase, setPhase] = useState<'pause' | 'deleting' | 'typing'>('pause');
  // Only the very first "pause" (before this word has ever cycled) gets
  // the extra startDelayMs — every pause after that is the plain pauseMs,
  // so the offset staggers the two words apart without permanently
  // slowing this one down relative to the other.
  const isFirstPause = useRef(true);

  useEffect(() => {
    if (phase === 'pause') {
      const delay = isFirstPause.current ? pauseMs + startDelayMs : pauseMs;
      isFirstPause.current = false;
      const t = setTimeout(() => setPhase('deleting'), delay);
      return () => clearTimeout(t);
    }
    if (phase === 'deleting') {
      if (text.length === 0) {
        setWordIndex((i) => (i + 1) % words.length);
        setPhase('typing');
        return;
      }
      const t = setTimeout(() => setText(text.slice(0, -1)), deleteMs);
      return () => clearTimeout(t);
    }
    const target = words[wordIndex];
    if (text.length === target.length) {
      const t = setTimeout(() => setPhase('pause'), 0);
      return () => clearTimeout(t);
    }
    const t = setTimeout(
      () => setText(target.slice(0, text.length + 1)),
      typeMs,
    );
    return () => clearTimeout(t);
  }, [phase, text, wordIndex, words, typeMs, deleteMs, pauseMs, startDelayMs]);

  return text;
}

function CyclingWord({
  words,
  startDelayMs = 0,
  pauseMs = 1800,
}: {
  words: string[];
  startDelayMs?: number;
  pauseMs?: number;
}) {
  // A fixed startDelayMs alone would just phase-shift these by a
  // constant forever, staying in that same relative offset every cycle.
  // Giving the two instances slightly different pauseMs as well means
  // their cycle lengths differ, so the two drift in and out of sync
  // over time instead of locking into one fixed relationship.
  const text = useCyclingWord(words, 130, 80, pauseMs, startDelayMs);
  return (
    <span className="inline-block whitespace-nowrap">
      {text}
      <span
        className="ml-[2px] inline-block h-[0.85em] w-[2px] bg-current align-[-0.12em]"
        style={{ animation: 'cursor-blink 1s step-end infinite' }}
      />
    </span>
  );
}

function FaqItem({ q, a }: { q: string; a: string }) {
  const [open, setOpen] = useState(false);
  return (
    <div className="border-b border-white/10">
      <button
        type="button"
        onClick={() => setOpen((v) => !v)}
        aria-expanded={open}
        className="flex w-full items-center justify-between gap-[var(--space-6)] bg-transparent py-[var(--space-7)] text-left text-[16px] font-normal text-white"
      >
        <span className={open ? 'text-white' : 'text-white/85'}>{q}</span>
        <span
          className={`grid h-[28px] w-[28px] flex-shrink-0 place-items-center rounded-full border border-white/20 text-white transition-transform duration-150 ${open ? 'rotate-45' : ''}`}
        >
          <svg
            width="12"
            height="12"
            viewBox="0 0 12 12"
            fill="none"
            aria-hidden="true"
          >
            <path
              d="M6 1V11M1 6H11"
              stroke="currentColor"
              strokeWidth="1.4"
              strokeLinecap="round"
            />
          </svg>
        </span>
      </button>
      {open && (
        <p className="m-0 max-w-[820px] pb-[var(--space-7)] text-[14px] font-light leading-relaxed text-white/55">
          {a}
        </p>
      )}
    </div>
  );
}

export function LandingPage() {
  // The trade app's body is a fixed-height, overflow:hidden single
  // screen (app.css) ;right for that terminal UI, wrong for this
  // marketing page, which is taller than one viewport. An inner
  // `h-screen overflow-y-auto` div "fixed" scrolling but only inside its
  // own box, leaving body's real (black) background exposed below it —
  // this instead makes the real page scroll for as long as this
  // component is mounted, restoring body's own rules on unmount.
  useEffect(() => {
    const { style } = document.body;
    const prevOverflow = style.overflow;
    const prevHeight = style.height;
    style.overflow = 'auto';
    style.height = 'auto';
    return () => {
      style.overflow = prevOverflow;
      style.height = prevHeight;
    };
  }, []);

  return (
    <div className="w-full bg-[#f4f4f7] text-[#0c0c14]">
      {/* Nav */}
      <header className="flex items-center justify-between gap-[var(--space-6)] px-[var(--space-9)] py-[var(--space-7)] sm:px-[32px]">
        <Link
          to="/"
          className="flex flex-shrink-0 items-center gap-[var(--space-4)] no-underline"
        >
          <img
            src="/cerdic-logo.png"
            alt="Cerdic"
            className="h-24 w-24 object-contain"
          />
          <span className="text-2xl font-medium tracking-tight text-[#0c0c14]">
            Cerdic
          </span>
        </Link>

        <nav className="hidden items-center gap-[2px] rounded-full border border-black/[0.06] bg-black/[0.03] p-[4px] md:flex">
          <a
            href="#markets"
            className="rounded-full bg-white px-[var(--space-6)] py-[var(--space-3)] text-[13px] font-medium text-[#0c0c14] no-underline shadow-[0_1px_2px_rgba(0,0,0,0.06)]"
          >
            Trade
          </a>
          <a
            href="https://cerdicxyz.github.io"
            target="_blank"
            rel="noreferrer"
            className="rounded-full px-[var(--space-6)] py-[var(--space-3)] text-[13px] font-normal text-black/55 no-underline transition-colors duration-150 hover:text-black/80"
          >
            Docs
          </a>
          <Link
            to="/agents"
            className="rounded-full px-[var(--space-6)] py-[var(--space-3)] text-[13px] font-normal text-black/55 no-underline transition-colors duration-150 hover:text-black/80"
          >
            Agents
          </Link>
          <Link
            to="/agents"
            className="rounded-full px-[var(--space-6)] py-[var(--space-3)] text-[13px] font-normal text-black/55 no-underline transition-colors duration-150 hover:text-black/80"
          >
            Faq
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
      <section className="relative flex flex-col items-center px-[var(--space-9)] pb-[40px] pt-[var(--space-7)] text-center sm:px-[48px]">
        <h1 className="m-0 mb-[var(--space-5)] max-w-[680px] text-[clamp(24px,4vw,44px)] font-semibold leading-[1.12] tracking-[-0.01em] text-[#0c0c14]">
          <CyclingWord words={['Sealed', 'Private']} pauseMs={3600} /> order
          flow,
          <br /> settled{' '}
          <CyclingWord
            words={['on-chain', 'on arc']}
            startDelayMs={1600}
            pauseMs={4400}
          />
        </h1>

        <p className="m-0 mb-[var(--space-6)] max-w-[480px] text-[14px] font-normal leading-relaxed text-black/55">
          Private perpetuals across FX, crypto, and commodities, matched in a
          Trusted Execution Environment(TEE) and settled on-chain.
        </p>

        <Link
          to="/trade"
          className="mx-auto inline-flex items-center gap-[var(--space-5)] rounded-full bg-accent px-[var(--space-9)] py-[var(--space-6)] text-[17px] font-normal text-white no-underline transition-colors duration-150 hover:bg-accent-strong"
        >
          Start trading
        </Link>
      </section>

      {/* Live app preview ;desktop screenshot with the mobile PWA
          mocked up in an iPhone frame overlapping its bottom-right
          corner, same composition as Liquid's own landing page. */}
      <section className="w-full px-[var(--space-8)] pb-[80px] sm:px-[20px] sm:pb-[110px] md:pb-[140px]">
        <div className="relative mx-auto max-w-[1320px]">
          <div
            className="overflow-hidden rounded-[20px] border border-white/[0.06] bg-[#0c0c0f]"
            style={{ boxShadow: '0 12px 32px rgba(0,0,0,0.22)' }}
          >
            <div className="flex items-center gap-[var(--space-5)] border-b border-white/[0.06] bg-[#141417] px-[18px] py-[12px]">
              <div className="flex flex-shrink-0 items-center gap-[7px]">
                <span className="h-[11px] w-[11px] rounded-full bg-white/20" />
                <span className="h-[11px] w-[11px] rounded-full bg-white/20" />
                <span className="h-[11px] w-[11px] rounded-full bg-white/20" />
              </div>
            </div>
            <img
              src="/preview.png"
              alt="Cerdic trading app preview"
              className="w-full object-cover object-top"
            />
          </div>

          <div className="absolute -bottom-[15px] -right-[20px] w-[210px] sm:-bottom-[25px] sm:-right-[30px] sm:w-[265px] md:-bottom-[35px] md:-right-[40px] md:w-[310px]">
            {/* Pre-composited with Pillow (see app/public/landing), not
                layered in CSS: the screenshot is resized to cover the
                mockup's exact transparent screen-hole (measured via
                flood-fill + visual mask verification) and pasted in,
                then the mockup frame is alpha-composited on top into one
                flat PNG. No runtime aspect-ratio/inset math to get
                wrong. */}
            <img
              src="/landing/iphone-mockup-composited.png"
              alt="Cerdic mobile app preview"
              className="w-full"
            />
          </div>
        </div>
      </section>

      {/* Feature grid ;dark band, boxes intentionally empty for now
          (illustrations land later), repeated twice per explicit
          request. */}
      <section className="w-full bg-black px-[var(--space-8)] py-[80px] sm:px-[48px]">
        <h2 className="m-0 mb-[var(--space-9)] max-w-[900px] text-[clamp(22px,3.2vw,36px)] font-medium leading-[1.25] tracking-[-0.01em] text-white">
          Trading, simplified.{' '}
          <span className="text-white/45">
            Pick a market, choose a direction, set your multiplier, and
            you&apos;re set.
          </span>
        </h2>

        <div className="mx-auto grid max-w-[1320px] grid-cols-1 gap-[var(--space-7)] md:grid-cols-3">
          <div>
            <div
              className="aspect-[4/3] rounded-[14px] border border-white/[0.08] bg-[#141417]"
              style={{ boxShadow: 'rgba(255,255,255,0.06) 0 1px 0 0 inset' }}
            />
            <h3 className="m-0 mb-[var(--space-3)] mt-[var(--space-6)] text-[17px] font-medium text-white">
              Go long. Go short. Privately.
            </h3>
            <p className="m-0 text-[13px] font-light leading-relaxed text-white/45">
              Think an asset will rise? Go long. Think it&apos;ll fall? Go
              short. That&apos;s the core of every trade.
            </p>
          </div>

          <div>
            <div
              className="aspect-[4/3] rounded-[14px] border border-white/[0.08] bg-[#141417]"
              style={{ boxShadow: 'rgba(255,255,255,0.06) 0 1px 0 0 inset' }}
            />
            <h3 className="m-0 mb-[var(--space-3)] mt-[var(--space-6)] text-[17px] font-medium text-white">
              Up to 50x multipliers.
            </h3>
            <p className="m-0 text-[13px] font-light leading-relaxed text-white/45">
              Put up $100, control a $5,000 position. Set your multiplier from
              1x to 50x.
            </p>
          </div>

          <div>
            <div
              className="aspect-[4/3] rounded-[14px] border border-white/[0.08] bg-[#141417]"
              style={{ boxShadow: 'rgba(255,255,255,0.06) 0 1px 0 0 inset' }}
            />
            <h3 className="m-0 mb-[var(--space-3)] mt-[var(--space-6)] text-[17px] font-medium text-white">
              Close anytime. No waiting.
            </h3>
            <p className="m-0 text-[13px] font-light leading-relaxed text-white/45">
              No lock-ups, no settlement periods. Close your position in
              seconds, 24/7.
            </p>
          </div>
        </div>
      </section>

      {/* FAQ */}
      <section className="w-full bg-black px-[var(--space-8)] py-[80px] sm:px-[48px]">
        <div className="mx-auto max-w-[820px]">
          <h2 className="m-0 mb-[var(--space-8)] text-[clamp(24px,3.6vw,40px)] font-semibold tracking-[-0.01em] text-white">
            Frequently Asked Questions
          </h2>
          <div>
            {FAQ_ITEMS.map((item) => (
              <FaqItem key={item.q} q={item.q} a={item.a} />
            ))}
          </div>
        </div>
      </section>

      {/* Footer ;light theme, matching the rest of the page (the
          feature grid/FAQ above stay a dark band on purpose, but the
          footer itself isn't part of that contrast device). */}
      <footer className="w-full overflow-hidden bg-[#f4f4f7] px-[var(--space-8)] pb-[40px] pt-[80px] sm:px-[48px]">
        <div className="mx-auto grid max-w-[1320px] grid-cols-2 gap-[var(--space-8)] pb-[64px] sm:grid-cols-4">
          <div className="flex flex-col gap-[var(--space-5)]">
            <span className="text-[11px] font-medium uppercase tracking-[0.08em] text-black/35">
              Product
            </span>
            <Link
              to="/trade"
              className="text-[13px] text-black/70 no-underline hover:text-black"
            >
              Trade
            </Link>
            <Link
              to="/agents"
              className="text-[13px] text-black/70 no-underline hover:text-black"
            >
              Agents
            </Link>
            <span
              className="cursor-not-allowed text-[13px] text-black/30"
              title="Markets ;coming soon"
            >
              Markets
            </span>
          </div>

          <div className="flex flex-col gap-[var(--space-5)]">
            <span className="text-[11px] font-medium uppercase tracking-[0.08em] text-black/35">
              Company
            </span>
            <span
              className="cursor-not-allowed text-[13px] text-black/30"
              title="About ;coming soon"
            >
              About
            </span>
            <span
              className="cursor-not-allowed text-[13px] text-black/30"
              title="Blog ;coming soon"
            >
              Blog
            </span>
            <span
              className="cursor-not-allowed text-[13px] text-black/30"
              title="Careers ;coming soon"
            >
              Careers
            </span>
          </div>

          <div className="flex flex-col gap-[var(--space-5)]">
            <span className="text-[11px] font-medium uppercase tracking-[0.08em] text-black/35">
              Resources
            </span>
            <a
              href="https://cerdicxyz.github.io"
              target="_blank"
              rel="noreferrer"
              className="text-[13px] text-black/70 no-underline hover:text-black"
            >
              Docs
            </a>
            <span
              className="cursor-not-allowed text-[13px] text-black/30"
              title="Support ;coming soon"
            >
              Support
            </span>
            <span
              className="cursor-not-allowed text-[13px] text-black/30"
              title="Status ;coming soon"
            >
              Status
            </span>
          </div>

          <div className="flex flex-col gap-[var(--space-5)]">
            <span className="text-[11px] font-medium uppercase tracking-[0.08em] text-black/35">
              Legal
            </span>
            <span
              className="cursor-not-allowed text-[13px] text-black/30"
              title="Terms ;coming soon"
            >
              Terms
            </span>
            <span
              className="cursor-not-allowed text-[13px] text-black/30"
              title="Privacy ;coming soon"
            >
              Privacy
            </span>
          </div>
        </div>

        <div className="mx-auto max-w-[1320px] border-t border-black/10 py-[var(--space-7)] text-[12px] leading-relaxed text-black/40">
          <p className="m-0 mb-(--space-4) max-w-[780px]">
            Cerdic is a perpetual futures venue. Trading perpetuals involves
            substantial risk of loss and is not suitable for every investor —
            leveraged positions can be liquidated in full. Order matching occurs
            inside a trusted execution environment; settlement is recorded
            on-chain and is final once confirmed. Nothing on this page is
            investment advice.
          </p>
          <p className="m-0">© 2026 Cerdic. All rights reserved.</p>
        </div>

        <div className="mx-auto max-w-[1320px] pb-[var(--space-8)]">
          <Link
            to="/trade"
            className="inline-flex items-center gap-[var(--space-5)] rounded-full bg-accent px-[var(--space-9)] py-[var(--space-6)] text-[17px] font-normal text-white no-underline transition-colors duration-150 hover:bg-accent-strong"
          >
            Start trading
          </Link>
        </div>

        {/* Crest + wordmark, same "huge logotype bleeding off the
            footer" move as Robinhood's own site ;here it's the crest
            shifted toward the left edge with "Cerdic" set big beside it,
            rather than one centered wordmark. */}
        <div className="mx-auto flex max-w-330 w-full items-center justify-start gap-(--space-4) overflow-hidden">
          <img
            src="/sheild-cerdic.png"
            alt="Cerdic crest"
            loading="lazy"
            className="w-[50%] max-w-125 shrink-0 opacity-90 sm:w-[34%]"
          />
          <span
            className="min-w-0 flex-1 select-none overflow-hidden text-left text-[clamp(80px,19vw,225px)] font-black leading-none tracking-[-0.03em] text-[#0c0c14]"
            aria-hidden="true"
          >
            C e r d i c
          </span>
        </div>
      </footer>
    </div>
  );
}
