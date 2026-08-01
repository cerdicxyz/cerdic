import { createContext, useContext, useEffect, useState, type ReactNode } from 'react';

// Ported from ~/work/cer-perp's toast system: a module-level singleton
// with a plain pub/sub listener list, not React Context, for the
// trigger side — `toast.success(...)` needs to be callable from
// anywhere (an event handler, a promise chain) without a hook, while
// `ToastProvider`/`useToast` exist only to subscribe the render tree to
// that same singleton state.

export interface ToastAction {
  label: string;
  onClick: () => void;
}

export interface Toast {
  id: string;
  type: 'success' | 'info' | 'warning' | 'error' | 'progress';
  title: string;
  description?: ReactNode;
  progress?: number;
  action?: ToastAction;
  duration?: number | null;
}

type ToastListener = (toasts: Toast[]) => void;
let listeners: ToastListener[] = [];
let currentToasts: Toast[] = [];

function emit() {
  listeners.forEach((l) => l([...currentToasts]));
}

function push(item: Toast) {
  currentToasts = [...currentToasts, item];
  emit();
  return item.id;
}

function nextId() {
  return Math.random().toString(36).slice(2, 9);
}

type ToastOptions = Omit<Partial<Toast>, 'id' | 'type' | 'title' | 'description'>;

export const toast = {
  success: (title: string, description?: ReactNode, options?: ToastOptions) =>
    push({ id: nextId(), type: 'success', title, description, duration: 5000, ...options }),

  info: (title: string, description?: ReactNode, options?: ToastOptions) =>
    push({ id: nextId(), type: 'info', title, description, duration: 5000, ...options }),

  warning: (title: string, description?: ReactNode, options?: ToastOptions) =>
    push({ id: nextId(), type: 'warning', title, description, duration: 6000, ...options }),

  error: (title: string, description?: ReactNode, options?: ToastOptions) =>
    push({ id: nextId(), type: 'error', title, description, duration: null, ...options }), // sticky by default

  progress: (
    title: string,
    progress: number,
    description?: ReactNode,
    options?: Omit<ToastOptions, 'progress'>,
  ) => push({ id: nextId(), type: 'progress', title, description, progress, duration: null, ...options }),

  update: (id: string, updates: Partial<Omit<Toast, 'id'>>) => {
    currentToasts = currentToasts.map((t) => (t.id === id ? { ...t, ...updates } : t));
    emit();
  },

  dismiss: (id: string) => {
    currentToasts = currentToasts.filter((t) => t.id !== id);
    emit();
  },
};

interface ToastContextValue {
  toasts: Toast[];
  dismiss: (id: string) => void;
}

const ToastContext = createContext<ToastContextValue | undefined>(undefined);

export function ToastProvider({ children }: { children: ReactNode }) {
  const [toasts, setToasts] = useState<Toast[]>(currentToasts);

  useEffect(() => {
    listeners.push(setToasts);
    setToasts([...currentToasts]);
    return () => {
      listeners = listeners.filter((l) => l !== setToasts);
    };
  }, []);

  return <ToastContext.Provider value={{ toasts, dismiss: toast.dismiss }}>{children}</ToastContext.Provider>;
}

export function useToast() {
  const context = useContext(ToastContext);
  if (!context) throw new Error('useToast must be used within a ToastProvider');
  return context;
}
