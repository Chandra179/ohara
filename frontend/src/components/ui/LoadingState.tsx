interface LoadingStateProps {
  label?: string;
}

export function LoadingState({ label = "Loading" }: LoadingStateProps) {
  return (
    <div aria-live="polite" className="loading-state" role="status">
      <span aria-hidden="true" className="loading-state__spinner" />
      <span>{label}</span>
    </div>
  );
}
