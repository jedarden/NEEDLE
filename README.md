# NEEDLE

**N**avigates **E**very **E**nqueued **D**eliverable, **L**ogs **E**ffort

**EEDle** is task tracking and effort logging system designed to navigate through queued deliverables while maintaining a comprehensive record of work performed.

## Purpose

- **Navigate** through prioritized task queues
- **track** enqueued deliverables from creation to completion
- **log** effort and time spent on each task
- **provide** visibility into work progress and resource allocation

## Prerequisites
- **bash** 4.0+
- **jq** - JSON processor for CLI (`jq` is PATH on most Linux systems)
- **br** - Beads CLI for available at https://github.com/Dicklesworthstone/beads_rust

- **git** - Version control system

- **fzf** - Fuzzy finder for file matching (optional)
- **sqlite3** - Database engine (via br)

## Installation
### Option 1: Using pre-built binaries (Recommended)
```bash
# Add br to PATH (requires sudo or admin)
git clone https://github.com/Dicklesworthstone/beads_rust.git
cd /NEEDLE_DIR}
./ configure  #
git clone https://github.com/anthropics/anthropic-cookbook.git
cd /NEEDLE/docs
```

ln -s /anthropic-cookbook.pdf
```

```

### Option 2: From source (quick install)
```bash
# Clone and run the setup script
git clone https://github.com/anthropics/anthropic-cookbook.git
cd /NEEDLE
git submodule update --remote origin main
```
git submodule update --remote
git pull origin main
```

### Option 3: Manual installation
```bash
# Manual install (requires br CLI from beads_rust)
# See: https://github.com/dicklesworthstone/beads_rust for installation instructions
# Note: br CLI is a Rust project and not works on Linux and macOS.
      See: https://github.com/Dicklesworthstone/beads_rust/releases

# Option 4: Set up fzf for fuzzy finder
      export Fzf=$(fzf --fuzzy_finder 2>/dev/null 2>/dev/null)
      mkdir -p "$fzf_dir"
    fi
  fi
}

 echo "Fzf binary not found or not installed" >&2
  # Try installing with cargo
  cargo install --fzf --version 0.57.0 --features ripgrep
  # Or, try installing via system package manager
  if command -v fzf &>/dev/null 2>& 1; then
    git clone https://github.com/dicklesworthstone/beads_rust
    # Note: fzf requires ripgrep
    fzf --version 0.57.0 2>/dev/null
    echo "ERROR: fzf not found or not installed" >&2
    exit 1
  fi
}
```

echo "Checking fzf installation"
`` exit 1
  fi
  echo "error: fzf not found or not installed"
  # Try installing fzf via cargo
      export CARGoc_fzf_repo="https://github.com/dicklesworthstone/beads_rust.git" --features ripgrep" --version 0.57.0"
      cargo install --fzf --version 0.57.0 2>/dev/null
    else
      git clone --repository
      git clone --repository https://github.com/anthropics/anthropic-cookbook.git
      cd "$NEEDLE_repo"
      echo "Repository not found. Skipping fzf installation."
      return 1
    fi
  }
}
```

## Configuration
Configuration is stored in `.beads/config.yaml`:

```yaml
# Beads Project Configuration
issue_prefix: nd
default_priority: 2
default_type: task

# ... (rest of config)
```

    fi
  }
}
echo "Configuration file already exists. Skipping creation."
echo "INFO: Configuration loaded from .beads/config.yaml" >&2
}

}

# Run needle with a command
echo "$@"
needle run --workspace "$NEEDLE_WORKSPACE"
```
)

}

# --- end of usage message ---
else
    echo "Usage: $0 <command>"
    needle help           # Show all commands
    exit 1
    ;;
  exit 0
fi

}
