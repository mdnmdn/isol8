#!/bin/bash
set -e

# isol8 Bash Installer
# Downloads and installs the latest release of isol8 from GitHub
#
# Usage examples:
# ./install.sh                     # Install latest version
# ./install.sh --version 0.1.0     # Install specific version
# ./install.sh -v v0.1.0           # Install specific version (v prefix auto-removed)
# ./install.sh --install-dir /usr/local/bin # Install to custom directory
# ./install.sh --help              # Show help message

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Configuration
REPO="mdnmdn/isol8"
BINARY_NAME="isol8"
INSTALL_DIR="$HOME/.local/bin"
VERSION=""

# Parse command line arguments
while [[ $# -gt 0 ]]; do
  case $1 in
    --version|-v)
      VERSION="$2"
      shift 2
      ;;
    --install-dir|-d)
      INSTALL_DIR="$2"
      shift 2
      ;;
    --help|-h)
      script_name="$(basename "${BASH_SOURCE[0]:-$0}")"
      if [[ "${script_name}" == "bash" || "${script_name}" == "stdin" ]]; then
        script_name="install.sh"
      fi
      echo "Usage: ${script_name} [OPTIONS]"
      echo "Options:"
      echo "  -v, --version VERSION  Specify version to install (e.g., 0.1.0 or v0.1.0 - v prefix auto-removed)"
      echo "  -d, --install-dir DIR  Specify installation directory (default: \$HOME/.local/bin)"
      echo "  -h, --help             Show this help message"
      exit 0
      ;;
    *)
      echo "Unknown option: $1"
      echo "Use --help for usage information"
      exit 1
      ;;
  esac
done

# Functions
log_info() {
  echo -e "${BLUE}[INFO]${NC} $1"
}

log_success() {
  echo -e "${GREEN}[SUCCESS]${NC} $1"
}

log_warning() {
  echo -e "${YELLOW}[WARNING]${NC} $1"
}

log_error() {
  echo -e "${RED}[ERROR]${NC} $1"
  exit 1
}

# Detect OS and architecture
detect_platform() {
  local os arch
  case "$(uname -s)" in
    Darwin)
      os="macos"
      ;;
    Linux)
      os="linux"
      ;;
    *)
      log_error "Unsupported operating system: $(uname -s)"
      ;;
  esac

  case "$(uname -m)" in
    x86_64|amd64)
      arch="x64"
      ;;
    aarch64|arm64)
      arch="arm64"
      ;;
    *)
      log_error "Unsupported architecture: $(uname -m)"
      ;;
  esac

  echo "${os}-${arch}"
}

# Get latest release version from GitHub or use provided version
get_version() {
  local provided_version="$1"
  if [[ -n "${provided_version}" ]]; then
    # Remove 'v' prefix if present
    if [[ "${provided_version}" = v* ]]; then
      provided_version="${provided_version#v}"
    fi
    log_info "Using specified version: ${provided_version}" >&2
    echo "${provided_version}"
    return
  fi

  log_info "Fetching latest release information..." >&2
  if command -v curl >/dev/null 2>&1; then
    curl -s "https://api.github.com/repos/${REPO}/releases/latest" | grep '"tag_name":' | sed -E 's/.*"([^"]+)".*/\1/' | sed 's/^v//'
  elif command -v wget >/dev/null 2>&1; then
    wget -qO- "https://api.github.com/repos/${REPO}/releases/latest" | grep '"tag_name":' | sed -E 's/.*"([^"]+)".*/\1/' | sed 's/^v//'
  else
    log_error "Neither curl nor wget is available. Please install one of them."
  fi
}

# Download and install
download_and_install() {
  local platform="$1"
  local version="$2"
  local download_url="https://github.com/${REPO}/releases/download/v${version}/${platform}.zip"
  local temp_dir=$(mktemp -d)
  local zip_file="${temp_dir}/${platform}.zip"

  log_info "Downloading ${BINARY_NAME} ${version} for ${platform}..."

  # Download the zip file
  if command -v curl >/dev/null 2>&1; then
    curl -L -o "${zip_file}" "${download_url}" || log_error "Failed to download ${download_url}"
  elif command -v wget >/dev/null 2>&1; then
    wget -O "${zip_file}" "${download_url}" || log_error "Failed to download ${download_url}"
  else
    log_error "Neither curl nor wget is available. Please install one of them."
  fi

  # Verify download
  if [[ ! -f "${zip_file}" ]] || [[ ! -s "${zip_file}" ]]; then
    log_error "Downloaded file is missing or empty"
  fi

  log_info "Extracting ${BINARY_NAME}..."

  # Extract the binary
  if command -v unzip >/dev/null 2>&1; then
    unzip -q "${zip_file}" -d "${temp_dir}" || log_error "Failed to extract ${zip_file}"
  else
    log_error "unzip is not available. Please install unzip."
  fi

  # Find the binary in the extracted files
  local binary_path
  binary_path=$(find "${temp_dir}" -name "${BINARY_NAME}" -type f | head -1)
  if [[ -z "${binary_path}" ]]; then
    log_error "Binary ${BINARY_NAME} not found in the downloaded archive"
  fi

  log_info "Installing ${BINARY_NAME} to ${INSTALL_DIR}..."

  # Create install directory if it doesn't exist
  mkdir -p "${INSTALL_DIR}"

  # Install the binary
  cp "${binary_path}" "${INSTALL_DIR}/${BINARY_NAME}"
  chmod +x "${INSTALL_DIR}/${BINARY_NAME}"

  # Cleanup
  rm -rf "${temp_dir}"

  log_success "${BINARY_NAME} ${version} installed successfully to ${INSTALL_DIR}/${BINARY_NAME}"
}

# Check if binary is in PATH
check_path() {
  if echo ":${PATH}:" | grep -q ":${INSTALL_DIR}:"; then
    log_success "${INSTALL_DIR} is already in your PATH"
  else
    log_warning "${INSTALL_DIR} is not in your PATH"
    echo ""
    echo "You can still run isol8 directly from:"
    echo "  ${INSTALL_DIR}/${BINARY_NAME}"
    echo ""
    echo "To use '${BINARY_NAME}' without full path, choose one option:"
    echo ""
    echo "1) Add ${INSTALL_DIR} to PATH:"
    echo "   export PATH=\"\$PATH:${INSTALL_DIR}\""
    echo ""
    case "$(basename "${SHELL:-}")" in
      zsh)
        echo "   Persist for zsh:"
        echo "   echo 'export PATH=\"\$PATH:${INSTALL_DIR}\"' >> ~/.zshrc"
        echo "   source ~/.zshrc"
        ;;
      bash)
        echo "   Persist for bash:"
        echo "   echo 'export PATH=\"\$PATH:${INSTALL_DIR}\"' >> ~/.bashrc"
        echo "   source ~/.bashrc"
        ;;
      *)
        echo "   Persist for bash:"
        echo "   echo 'export PATH=\"\$PATH:${INSTALL_DIR}\"' >> ~/.bashrc"
        echo "   source ~/.bashrc"
        echo ""
        echo "   Persist for zsh:"
        echo "   echo 'export PATH=\"\$PATH:${INSTALL_DIR}\"' >> ~/.zshrc"
        echo "   source ~/.zshrc"
        ;;
    esac
    echo ""
    echo "2) Create a symlink in a PATH directory:"
    echo "   ln -sf \"${INSTALL_DIR}/${BINARY_NAME}\" /usr/local/bin/${BINARY_NAME}"
  fi
}

# Verify installation
verify_installation() {
  if [[ -x "${INSTALL_DIR}/${BINARY_NAME}" ]]; then
    local version_output
    version_output=$("${INSTALL_DIR}/${BINARY_NAME}" @version 2>&1 | head -1 || echo "Unable to get version")
    log_success "Installation verified: ${version_output}"
  else
    log_error "Installation verification failed: ${INSTALL_DIR}/${BINARY_NAME} is not executable"
  fi
}

# Main installation process
main() {
  echo "isol8 Installer"
  echo "==============="
  echo ""

  # Check prerequisites
  if ! command -v unzip >/dev/null 2>&1; then
    log_error "unzip is required but not installed. Please install it first."
  fi

  if ! command -v curl >/dev/null 2>&1 && ! command -v wget >/dev/null 2>&1; then
    log_error "Either curl or wget is required but neither is installed. Please install one of them."
  fi

  # Detect platform
  local platform
  platform=$(detect_platform)
  log_info "Detected platform: ${platform}"

  # Get version (latest or specified)
  local version
  version=$(get_version "${VERSION}")
  if [[ -z "${version}" ]]; then
    log_error "Failed to get version information"
  fi
  log_info "Version to install: ${version}"

  # Download and install
  download_and_install "${platform}" "${version}"

  # Check PATH
  check_path

  # Verify installation
  verify_installation

  echo ""
  log_success "Installation complete!"
  echo ""
  echo "Installed binary:"
  echo "  ${INSTALL_DIR}/${BINARY_NAME}"
  echo ""
  echo "Quick check:"
  echo "  ${INSTALL_DIR}/${BINARY_NAME} @version"
  echo ""
  echo "Next steps:"
  echo "1. Run 'isol8 --help' to see available options"
  echo "2. Run 'isol8 --show-profiles' to list available sandbox profiles"
  echo ""
  echo "For more information, visit: https://github.com/${REPO}"
}

# Run the installer when executed (file or stdin), but not when sourced.
# This supports:
# - bash install.sh
# - cat install.sh | bash
# - curl ... | bash
if ! (return 0 2>/dev/null); then
  main "$@"
fi
