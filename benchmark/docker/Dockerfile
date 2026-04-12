# Stage 1: Build uppli-code on trixie (has GCC 14 / libstdc++ with __cxa_call_terminate)
FROM rust:latest AS builder

RUN apt-get update && apt-get install -y --no-install-recommends \
    libasound2-dev pkg-config cmake g++ \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build
COPY src-rust/ ./src-rust/

WORKDIR /build/src-rust
RUN cargo build --release --bin uppli-code
RUN ls -la target/release/uppli-code
# Copy the libstdc++ that uppli-code needs
RUN cp /usr/lib/aarch64-linux-gnu/libstdc++.so.6 /tmp/libstdcpp.so.6

# Stage 2: Python environment matching SWE-bench testbed for astropy
FROM python:3.11-bookworm

# System deps for astropy C extensions
RUN apt-get update && apt-get install -y --no-install-recommends \
    gcc g++ gfortran git make pkg-config \
    libffi-dev libssl-dev \
    && rm -rf /var/lib/apt/lists/*

# Install pinned deps matching SWE-bench testbed env
RUN pip install --no-cache-dir \
    "numpy==1.25.2" \
    "pyerfa==2.0.0.3" \
    "scipy==1.11.1" \
    "pyyaml==6.0.1" \
    "pytest==7.4.0" \
    "pytest-xdist==3.3.1" \
    "pytest-astropy==0.10.0" \
    "hypothesis==6.82.6" \
    "cython==0.29.35" \
    "extension-helpers==1.0.0" \
    "setuptools-scm==7.1.0" \
    "setuptools==67.6.1"

# Copy astropy repo and install it
COPY benchmark/docker/astropy_repo /testbed
WORKDIR /testbed

ENV SETUPTOOLS_SCM_PRETEND_VERSION=5.0.dev0
RUN git init && git add -A && git commit -m "init" --allow-empty 2>/dev/null || true
RUN sed -i 's/requires = \["setuptools"/requires = ["setuptools<68"/' pyproject.toml
RUN pip install --no-build-isolation -e ".[test]"
RUN python -c "import astropy; print('astropy', astropy.__version__)"

# Copy uppli-code binary + trixie's libstdc++ (has __cxa_call_terminate)
COPY --from=builder /build/src-rust/target/release/uppli-code /usr/local/bin/uppli-code
COPY --from=builder /tmp/libstdcpp.so.6 /usr/local/lib/libstdc++.so.6
RUN chmod +x /usr/local/bin/uppli-code
RUN ldconfig

# Copy UPPLI.md
COPY benchmark/docker/UPPLI.md /testbed/UPPLI.md

ENV HOME=/root
WORKDIR /testbed
CMD ["uppli-code", "--provider", "openrouter", "--print", "--output-format", "json"]
