import { Button } from "./Button";

interface ErrorStateProps {
  description: string;
  onRetry?: () => void;
  title?: string;
}

export function ErrorState({
  description,
  onRetry,
  title = "Something went wrong",
}: ErrorStateProps) {
  return (
    <div aria-live="assertive" className="error-state" role="alert">
      <h2>{title}</h2>
      <p>{description}</p>
      {onRetry ? (
        <Button onClick={onRetry} variant="secondary">
          Try again
        </Button>
      ) : null}
    </div>
  );
}
