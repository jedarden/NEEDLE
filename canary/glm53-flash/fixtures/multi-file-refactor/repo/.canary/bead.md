# canary-multi-file-refactor

Move the duplicated `format_status` implementation into `src/formatting.py`
and make both `src/config.py` and `src/report.py` use it. The gate must pass
and only those three source paths may change.
