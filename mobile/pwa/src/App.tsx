import { useState } from 'react';
import { BottomNav, type NavTab } from './components/BottomNav';
import { HomeScreen } from './components/HomeScreen';
import { TradeScreen } from './components/TradeScreen';
import { PortfolioScreen } from './components/PortfolioScreen';
import { TradeSheet } from './components/TradeSheet';

function App() {
  const [activeTab, setActiveTab] = useState<NavTab>('trade');
  const [tradeSheetOpen, setTradeSheetOpen] = useState(false);

  return (
    // Phone-width shell, enforced regardless of the actual browser
    // viewport — this is a mobile app mockup, not a responsive page, so
    // it should look like a phone screen even on a wide desktop window
    // with devtools closed, not just under a device-emulation toolbar.
    <div className="flex h-full w-full justify-center bg-[#050505]">
      <div className="relative flex h-full w-full max-w-[430px] flex-col overflow-hidden bg-surface-base text-text-primary shadow-[0_0_60px_rgba(0,0,0,0.6)] sm:border-x sm:border-border-subtle">
        {activeTab === 'home' && <HomeScreen />}
        {activeTab === 'trade' && <TradeScreen onOpenTradeSheet={() => setTradeSheetOpen(true)} />}
        {activeTab === 'portfolio' && <PortfolioScreen />}

        <BottomNav active={activeTab} onChange={setActiveTab} />

        {tradeSheetOpen && <TradeSheet onClose={() => setTradeSheetOpen(false)} />}
      </div>
    </div>
  );
}

export default App;
