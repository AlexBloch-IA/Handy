import React, { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { open } from "@tauri-apps/plugin-dialog";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { Upload } from "lucide-react";
import { Button } from "../../ui/Button";

const SUPPORTED_EXTENSIONS = [
  "m4a",
  "mp4",
  "mp3",
  "wav",
  "flac",
  "ogg",
  "oga",
  "aac",
  "caf",
];

interface FileDropZoneProps {
  onFiles: (paths: string[]) => void;
}

export const FileDropZone: React.FC<FileDropZoneProps> = ({ onFiles }) => {
  const { t } = useTranslation();
  const [isHovering, setIsHovering] = useState(false);

  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;

    getCurrentWebview()
      .onDragDropEvent((event) => {
        if (event.payload.type === "over") {
          setIsHovering(true);
        } else if (event.payload.type === "drop") {
          setIsHovering(false);
          onFiles(event.payload.paths);
        } else {
          setIsHovering(false);
        }
      })
      .then((fn) => {
        // Le composant peut avoir été démonté avant que l'écoute soit prête :
        // sans ce garde, l'écouteur survit au démontage.
        if (cancelled) {
          fn();
          return;
        }
        unlisten = fn;
      });

    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [onFiles]);

  const handleBrowse = useCallback(async () => {
    const selected = await open({
      multiple: true,
      filters: [
        { name: t("files.audioFilter"), extensions: SUPPORTED_EXTENSIONS },
      ],
    });
    if (!selected) return;
    onFiles(Array.isArray(selected) ? selected : [selected]);
  }, [onFiles, t]);

  return (
    <div
      className={`flex flex-col items-center justify-center gap-3 p-8 m-4 rounded-lg border-2 border-dashed transition-colors ${
        isHovering
          ? "border-logo-primary bg-logo-primary/10"
          : "border-mid-gray/30"
      }`}
    >
      <Upload width={28} height={28} className="opacity-60" />
      <p className="text-sm text-text/70 text-center">{t("files.dropHint")}</p>
      <Button variant="secondary" size="sm" onClick={handleBrowse}>
        {t("files.browse")}
      </Button>
    </div>
  );
};
