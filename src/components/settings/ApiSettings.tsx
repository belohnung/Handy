import React, { useCallback, useState } from "react";
import { useTranslation } from "react-i18next";
import { ToggleSwitch } from "../ui/ToggleSwitch";
import { SettingContainer } from "../ui/SettingContainer";
import { Input } from "../ui/Input";
import { useSettings } from "../../hooks/useSettings";

interface ApiSettingsProps {
  descriptionMode?: "inline" | "tooltip";
  grouped?: boolean;
}

export const ApiToggle: React.FC<ApiSettingsProps> = React.memo(
  ({ descriptionMode = "tooltip", grouped = false }) => {
    const { t } = useTranslation();
    const { getSetting, updateSetting, isUpdating } = useSettings();

    const enabled = getSetting("api_enabled") || false;

    return (
      <ToggleSwitch
        checked={enabled}
        onChange={(value) => updateSetting("api_enabled", value)}
        isUpdating={isUpdating("api_enabled")}
        label={t("settings.debug.api.enabled.label")}
        description={t("settings.debug.api.enabled.description")}
        descriptionMode={descriptionMode}
        grouped={grouped}
      />
    );
  },
);

export const ApiPortSetting: React.FC<ApiSettingsProps> = React.memo(
  ({ descriptionMode = "tooltip", grouped = false }) => {
    const { t } = useTranslation();
    const { getSetting, updateSetting, isUpdating } = useSettings();

    const storePort = getSetting("api_port") ?? 9876;
    const [localPort, setLocalPort] = useState<string>(String(storePort));

    // Keep local state in sync when store changes externally
    React.useEffect(() => {
      setLocalPort(String(storePort));
    }, [storePort]);

    const handleChange = useCallback(
      (event: React.ChangeEvent<HTMLInputElement>) => {
        setLocalPort(event.target.value);
      },
      [],
    );

    const handleBlur = useCallback(() => {
      const value = parseInt(localPort, 10);
      if (!isNaN(value) && value > 0 && value <= 65535) {
        updateSetting("api_port", value);
      } else {
        // Reset to store value if invalid
        setLocalPort(String(storePort));
      }
    }, [localPort, storePort, updateSetting]);

    const handleKeyDown = useCallback((event: React.KeyboardEvent) => {
      if (event.key === "Enter") {
        (event.target as HTMLInputElement).blur();
      }
    }, []);

    return (
      <SettingContainer
        title={t("settings.debug.api.port.title")}
        description={t("settings.debug.api.port.description")}
        descriptionMode={descriptionMode}
        grouped={grouped}
        layout="horizontal"
      >
        <Input
          type="number"
          min="1"
          max="65535"
          value={localPort}
          onChange={handleChange}
          onBlur={handleBlur}
          onKeyDown={handleKeyDown}
          disabled={isUpdating("api_port")}
          className="w-24"
        />
      </SettingContainer>
    );
  },
);
