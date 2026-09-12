import { useCallback, useEffect, useRef, useState, type ReactNode } from "react";
import { NotificationRegion } from "../components/ui/NotificationRegion";
import {
  NotificationContext,
  type Notification,
  type NotificationInput,
} from "./context";

const MAX_NOTIFICATIONS = 4;
const NOTIFICATION_DURATION_MS = 10_000;

interface NotificationProviderProps {
  children: ReactNode;
}

export function NotificationProvider({ children }: NotificationProviderProps) {
  const [notifications, setNotifications] = useState<Notification[]>([]);
  const nextId = useRef(0);
  const timers = useRef(new Map<number, ReturnType<typeof setTimeout>>());

  const notify = useCallback((notification: NotificationInput) => {
    const id = nextId.current++;
    setNotifications((current) => {
      const next = [...current, { ...notification, id }];
      const removed = next.slice(0, Math.max(0, next.length - MAX_NOTIFICATIONS));
      for (const removedNotification of removed) {
        const timer = timers.current.get(removedNotification.id);
        if (timer !== undefined) {
          clearTimeout(timer);
          timers.current.delete(removedNotification.id);
        }
      }
      return next.slice(-MAX_NOTIFICATIONS);
    });

    const timer = setTimeout(() => {
      timers.current.delete(id);
      setNotifications((current) => current.filter((item) => item.id !== id));
    }, NOTIFICATION_DURATION_MS);
    timers.current.set(id, timer);
  }, []);

  const dismiss = useCallback((id: number) => {
    const timer = timers.current.get(id);
    if (timer !== undefined) {
      clearTimeout(timer);
      timers.current.delete(id);
    }
    setNotifications((current) => current.filter((notification) => notification.id !== id));
  }, []);

  useEffect(() => {
    const activeTimers = timers.current;
    return () => {
      for (const timer of activeTimers.values()) {
        clearTimeout(timer);
      }
      activeTimers.clear();
    };
  }, []);

  return (
    <NotificationContext.Provider value={{ dismiss, notify, notifications }}>
      {children}
      <NotificationRegion />
    </NotificationContext.Provider>
  );
}
