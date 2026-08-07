import { TickerHeader } from './TickerHeader';
import { TimeframeTabs } from './TimeframeTabs';
import { MobileChart } from './MobileChart';
import { MarketTabs } from './MarketTabs';
import { MobileOrderBook } from './MobileOrderBook';
import { PositionTabs } from './PositionTabs';
import { TradeActionBar } from './TradeActionBar';

export function TradeScreen({ onOpenTradeSheet }: { onOpenTradeSheet: () => void }) {
  return (
    <div className="flex flex-1 flex-col overflow-hidden">
      <div className="flex flex-1 flex-col overflow-y-auto">
        <TickerHeader />
        <TimeframeTabs />
        <MobileChart />
        <MarketTabs />
        <MobileOrderBook />
      </div>
      <PositionTabs />
      <TradeActionBar onOpenTradeSheet={onOpenTradeSheet} />
    </div>
  );
}
