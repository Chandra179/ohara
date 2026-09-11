import { useCallback, useRef, useState, type ReactNode } from "react";
import { NotificationRegion } from "../components/ui/NotificationRegion";
import {
  NotificationContext,
  type Notification,
  type NotificationInput,
} from "./context";

const MAX_NOTIFICATIONS = 4;

interface NotificationProviderProps {
  children: ReactNode;
}

export function NotificationProvider({ children }: NotificationProviderProps) {
  const [notifications, setNotifications] = useState<Notification[]>([]);
  const nextId = useRef(0);

  const notify = useCallback((notification: NotificationInput) => {
    setNotifications((current) => [
      ...current,
      { ...notification, id: nextId.current++ },
    ].slice(-MAX_NOTIFICATIONS));
  }, []);

  const dismiss = useCallback((id: number) => {
    setNotifications((current) => current.filter((notification) => notification.id !== id));
  }, []);

  return (
    <NotificationContext.Provider value={{ dismiss, notify, notifications }}>
      {children}
      <NotificationRegion />
    </NotificationContext.Provider>
  );
}
