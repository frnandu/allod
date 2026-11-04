import useSWR from "swr";
import { fetcher } from "./api";

/** False while the recovery phrase backup has not been checked (SPEC §13.3); undefined while loading. */
export function useBackupVerified(): boolean | undefined {
  const { data } = useSWR<{ verified: boolean }>("/api/backup", fetcher);
  return data?.verified;
}
