import React, { useCallback, useEffect, useMemo } from "react";
import { useTranslation } from "react-i18next";
import { X } from "lucide-react";
import { toast } from "sonner";
import { commands } from "@/bindings";
import { useFileTranscriptionStore } from "../../../stores/fileTranscriptionStore";
import { SettingsGroup } from "../../ui";
import { Alert } from "../../ui/Alert";
import { Button } from "../../ui/Button";
import { FileDropZone } from "./FileDropZone";
import { FileJobRow } from "./FileJobRow";

export const FilesSettings: React.FC = () => {
  const { t } = useTranslation();
  const {
    jobs,
    selectedJobId,
    enqueueError,
    refresh,
    enqueue,
    cancel,
    select,
    clearEnqueueError,
    subscribe,
  } = useFileTranscriptionStore();

  useEffect(() => {
    refresh();

    let cancelled = false;
    let unlisten: (() => void) | undefined;

    subscribe().then((fn) => {
      // Le composant peut être démonté (changement d'onglet) avant que
      // l'écoute soit prête : sans ce garde, l'écouteur fuit et chaque
      // aller-retour sur l'onglet ajouterait un doublon.
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
  }, [refresh, subscribe]);

  const handleFiles = useCallback(
    (paths: string[]) => {
      enqueue(paths);
    },
    [enqueue],
  );

  const selectedJob = useMemo(
    () => jobs.find((j) => j.id === selectedJobId) ?? null,
    [jobs, selectedJobId],
  );

  const handleCopy = useCallback(async () => {
    if (!selectedJob?.transcript) return;
    try {
      await navigator.clipboard.writeText(selectedJob.transcript);
      toast.success(t("files.copied"));
    } catch (error) {
      console.error("Failed to copy transcript to clipboard:", error);
    }
  }, [selectedJob, t]);

  const handleReveal = useCallback(async () => {
    if (!selectedJob?.output_path) return;
    const result = await commands.revealTranscriptFile(selectedJob.output_path);
    if (result.status === "error") {
      console.error("Failed to reveal transcript file:", result.error);
      toast.error(t("files.revealError"));
    }
  }, [selectedJob, t]);

  return (
    <div className="max-w-3xl w-full mx-auto space-y-6">
      <SettingsGroup title={t("files.title")}>
        <FileDropZone onFiles={handleFiles} />
      </SettingsGroup>

      {enqueueError && (
        <div className="relative">
          <Alert variant="error">{enqueueError}</Alert>
          <button
            className="absolute top-3 end-3 p-1 rounded text-red-400 hover:bg-red-500/10 cursor-pointer"
            title={t("common.close")}
            onClick={clearEnqueueError}
          >
            <X width={14} height={14} />
          </button>
        </div>
      )}

      {jobs.length > 0 && (
        <SettingsGroup title={t("files.queue")}>
          <div className="flex flex-col p-2">
            {jobs.map((job) => (
              <FileJobRow
                key={job.id}
                job={job}
                isSelected={job.id === selectedJobId}
                onSelect={() => select(job.id)}
                onCancel={() => cancel(job.id)}
              />
            ))}
          </div>
        </SettingsGroup>
      )}

      {selectedJob?.status === "done" && (
        <SettingsGroup title={t("files.result")}>
          <div className="flex flex-col gap-3 p-4">
            <textarea
              readOnly
              value={selectedJob.transcript ?? ""}
              className="w-full h-64 p-3 text-sm rounded-lg bg-mid-gray/10 resize-none select-text cursor-text focus:outline-none"
            />
            <div className="flex gap-2">
              <Button variant="secondary" size="sm" onClick={handleCopy}>
                {t("files.copy")}
              </Button>
              {selectedJob.output_path && (
                <Button variant="secondary" size="sm" onClick={handleReveal}>
                  {t("files.reveal")}
                </Button>
              )}
            </div>
            {/* Un job « done » peut porter une erreur : le transcript est bon
                mais l'écriture du .txt a échoué. */}
            {selectedJob.error && (
              <p className="text-xs text-red-400">{selectedJob.error}</p>
            )}
          </div>
        </SettingsGroup>
      )}

      {selectedJob?.status === "failed" && (
        <Alert variant="error">
          {selectedJob.error ?? t("files.genericError")}
        </Alert>
      )}
    </div>
  );
};
