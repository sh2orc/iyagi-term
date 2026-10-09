import { t } from "../../i18n";
import { RpcClientError } from "../daemon/client";
import { claudeProviderErrorMessage } from "./claudeProvider";

/** Kept outside the React component module so Vite Fast Refresh stays valid. */
export function formatLaunchError(error: unknown): string {
  // 제공자 라우팅 거절(zai_key_missing 등)은 reason_code만으로 문구를 만든다 —
  // 데몬 message에 키·경로가 섞일 여지를 두지 않는다.
  const routed = claudeProviderErrorMessage(error);
  if (routed) return routed;
  if (error instanceof RpcClientError) {
    if (error.code === "CAPABILITY_UNAVAILABLE") {
      return t("managed.errorCapabilityUnavailable", { message: error.message });
    }
    return t("managed.errorLaunchFailed", { code: error.code, message: error.message });
  }
  return error instanceof Error && error.message ? error.message : t("managed.errorGeneric");
}
