import { useNotifications } from "../../notifications/useNotifications";
import { Button } from "./Button";

export function NotificationRegion() {
  const { dismiss, notifications } = useNotifications();

  if (notifications.length === 0) {
    return null;
  }

  return (
    <aside aria-label="Notifications" className="notification-region">
      {notifications.map((notification) => (
        <div
          aria-live={notification.tone === "error" ? "assertive" : "polite"}
          className={`notification notification--${notification.tone}`}
          key={notification.id}
          role={notification.tone === "error" ? "alert" : "status"}
        >
          <div className="notification__copy">
            <strong>{notification.title}</strong>
            <span>{notification.message}</span>
          </div>
          <Button
            aria-label={`Dismiss notification: ${notification.title}`}
            onClick={() => dismiss(notification.id)}
            variant="ghost"
          >
            Dismiss
          </Button>
        </div>
      ))}
    </aside>
  );
}
