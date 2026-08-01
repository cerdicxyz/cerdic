import { useState } from 'react';
import { Routes, Route, Navigate } from 'react-router';
import { Header } from './components/Header';
import { Sidebar } from './components/Sidebar';
import { TradePage } from './pages/TradePage';
import { PortfolioModal } from './components/PortfolioModal';
import { ToastProvider } from './toast/toast-context';
import { ToastContainer } from './toast/toast-container';

// App.tsx owns the shell shared across routes (Header/Sidebar/
// ToastContainer). Trade is a real route (react-router); Portfolio is a
// modal instead — reachable from Sidebar's Portfolio icon from wherever
// you are, not a page navigation. State lives here since Sidebar and the
// modal itself are siblings, not parent/child.

export default function App() {
  const [portfolioOpen, setPortfolioOpen] = useState(false);

  return (
    <ToastProvider>
      <div className="flex h-full flex-col overflow-hidden bg-surface-base">
        <ToastContainer />
        <Header />
        <div className="flex min-h-0 flex-1 overflow-hidden bg-surface-base">
          <Sidebar onOpenPortfolio={() => setPortfolioOpen(true)} portfolioOpen={portfolioOpen} />
          <Routes>
            <Route path="/" element={<Navigate to="/trade" replace />} />
            <Route path="/trade" element={<TradePage />} />
          </Routes>
        </div>
        <PortfolioModal open={portfolioOpen} onClose={() => setPortfolioOpen(false)} />
      </div>
    </ToastProvider>
  );
}
