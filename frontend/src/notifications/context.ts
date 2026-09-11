import { createContext } from "react";

export type NotificationTone = "error" | "info" | "success";

export interface NotificationInput {
  message: string;
  title: string;
  tone: NotificationTone;
}

export interface Notification extends NotificationInput {
  id: number;
}

export interface NotificationContextValue {
  dismiss: (id: number) => void;
  notify: (notification: NotificationInput) => void;
  notifications: Notification[];
}

export const NotificationContext = createContext<NotificationContextValue | null>(null);
