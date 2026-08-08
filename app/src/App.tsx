import { useState } from 'react';
import { Routes, Route } from 'react-router';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { PrivyProvider } from '@privy-io/react-auth';
import { Header } from './components/Header';
import { Sidebar } from './components/Sidebar';
import { TradePage } from './pages/TradePage';
import { AgentsPage } from './pages/AgentsPage';
import { LandingPage } from './pages/LandingPage';
import { PortfolioModal } from './components/PortfolioModal';
import { OnboardingModal } from './components/OnboardingModal';
import { ToastProvider } from './toast/toast-context';
import { ToastContainer } from './toast/toast-container';
import { WalletProvider } from './wallet/wallet-context';
import { privyAppId, privyClientId, privyConfig } from './wallet/privy';

// wagmi/Web3Auth/Kernel (wallet/wagmiConfig.ts, wallet/web3auth.ts,
// wallet/pimlicoWallet.ts) are no longer wired in here — Privy is the
// only wallet path (see wallet/privy.ts and wallet-context.tsx's module
// docs for why). Those files stay in the tree, unused, since they
// document a real, confirmed Arc Testnet infrastructure gap that's
// worth keeping the trail for, not because anything still imports them.
const queryClient = new QueryClient();

// App.tsx owns the shell shared across routes (Header/Sidebar/
// ToastContainer). Trade is a real route (react-router); Portfolio is a
// modal instead — reachable from Sidebar's Portfolio icon from wherever
// you are, not a page navigation. State lives here since Sidebar and the
// modal itself are siblings, not parent/child.

function AppShell({ portfolioOpen, setPortfolioOpen }: { portfolioOpen: boolean; setPortfolioOpen: (v: boolean) => void }) {
  return (
    <ToastProvider>
      <WalletProvider>
        <div className="flex h-full flex-col overflow-hidden bg-surface-base">
          <ToastContainer />
          <Header />
          <div className="flex min-h-0 flex-1 overflow-hidden bg-surface-base">
            <Sidebar onOpenPortfolio={() => setPortfolioOpen(true)} portfolioOpen={portfolioOpen} />
            <Routes>
              <Route path="/" element={<TradePage />} />
              <Route path="/trade" element={<TradePage />} />
              <Route path="/trade/:pair" element={<TradePage />} />
              <Route path="/agents" element={<AgentsPage />} />
            </Routes>
          </div>
          <PortfolioModal open={portfolioOpen} onClose={() => setPortfolioOpen(false)} />
          <OnboardingModal />
        </div>
      </WalletProvider>
    </ToastProvider>
  );
}

// Privy's OAuth redirect URL is configured as bare http://localhost:5174
// (root, no path) and reads privy_oauth_code/state straight off
// window.location.search on landing — a client-side <Navigate replace>
// at "/" would rewrite the URL and strip that query string in a race
// against Privy's own callback parsing, which is exactly what broke
// Google login the first time this was built. Checked once, synchronously,
// at mount (useState initializer, never an effect that would itself
// trigger a re-render/redirect) rather than every render: this only ever
// needs to be true for the single render right after Privy bounces back,
// and computing it fresh each render would flip it back to false as soon
// as something else (e.g. Privy's own SDK) touches the URL later in the
// same session.
function isPrivyOAuthCallback(): boolean {
  return typeof window !== 'undefined' && window.location.search.includes('privy_oauth_code');
}

export default function App() {
  const [portfolioOpen, setPortfolioOpen] = useState(false);
  const [oauthCallback] = useState(isPrivyOAuthCallback);

  return (
    <PrivyProvider appId={privyAppId} clientId={privyClientId} config={privyConfig}>
      <QueryClientProvider client={queryClient}>
        <Routes>
          {/* "/" is the marketing landing page (no Header/Sidebar/
              PortfolioModal chrome — a landing page isn't the trading
              terminal) EXCEPT immediately after a Privy OAuth bounce-back,
              which also lands on bare "/" — that one case still needs to
              render AppShell (whose own inner Routes resolves "/" to
              TradePage), not the landing page, or a mid-login user would
              land back on marketing copy instead of the terminal they were
              using. No redirect either way — just a different element
              rendered for the same URL, so the OAuth query string is never
              touched. */}
          <Route
            path="/"
            element={
              oauthCallback ? (
                <AppShell portfolioOpen={portfolioOpen} setPortfolioOpen={setPortfolioOpen} />
              ) : (
                <LandingPage />
              )
            }
          />
          <Route path="*" element={<AppShell portfolioOpen={portfolioOpen} setPortfolioOpen={setPortfolioOpen} />} />
        </Routes>
      </QueryClientProvider>
    </PrivyProvider>
  );
}
