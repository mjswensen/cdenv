FROM ubuntu

RUN apt-get update \
    && apt-get install -y sudo tmux \
    && rm -rf /var/lib/apt/lists/*

RUN usermod -aG sudo ubuntu && \
    echo "ubuntu ALL=(ALL) NOPASSWD:ALL" > /etc/sudoers.d/ubuntu
