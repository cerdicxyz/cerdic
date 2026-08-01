import { useEffect, useState } from 'react';
import { createPortal } from 'react-dom';
import { AnimatePresence } from 'framer-motion';
import { ToastCard } from './toast-card';
import { useToast } from './toast-context';

export function ToastContainer() {
  const { toasts, dismiss } = useToast();
  const [mounted, setMounted] = useState(false);

  useEffect(() => setMounted(true), []);

  if (!mounted) return null;

  return createPortal(
    <div className="pointer-events-none fixed right-[var(--space-4)] top-[64px] z-[999] flex w-[360px] flex-col items-end gap-[var(--space-3)]">
      <AnimatePresence mode="popLayout">
        {toasts.map((t) => (
          <ToastCard key={t.id} toast={t} onClose={dismiss} />
        ))}
      </AnimatePresence>
    </div>,
    document.body,
  );
}
