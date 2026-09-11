import { useCallback, useEffect, useState } from "react";

export type AsyncResource<T> =
  | { status: "error"; error: string }
  | { status: "loading" }
  | { data: T; status: "success" };

interface AsyncResourceResult<T> {
  reload: () => void;
  resource: AsyncResource<T>;
}

export function useAsyncResource<T>(load: () => Promise<T>): AsyncResourceResult<T> {
  const [resource, setResource] = useState<AsyncResource<T>>({ status: "loading" });
  const [requestNumber, setRequestNumber] = useState(0);

  useEffect(() => {
    let active = true;
    setResource({ status: "loading" });

    void load()
      .then((data) => {
        if (active) {
          setResource({ data, status: "success" });
        }
      })
      .catch((error: unknown) => {
        if (active) {
          setResource({ error: errorMessage(error), status: "error" });
        }
      });

    return () => {
      active = false;
    };
  }, [load, requestNumber]);

  const reload = useCallback(() => {
    setResource({ status: "loading" });
    setRequestNumber((current) => current + 1);
  }, []);

  return { reload, resource };
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : "The request could not be completed";
}
