FROM debian:13-slim
LABEL devcontainer.metadata='[{"id":"fixture-image","init":true,"capAdd":["NET_ADMIN","SYS_PTRACE"],"containerEnv":{"FROM_IMAGE":"yes","ORDER":"image"},"remoteEnv":{"IMAGE_REMOTE":"yes"},"mounts":[{"source":"image-source","target":"/merged","type":"volume"}],"forwardPorts":[7000,7001],"hostRequirements":{"cpus":1,"memory":"2gb"},"onCreateCommand":"printf image-metadata"}]'
CMD ["/bin/sh", "-c", "while sleep 3600; do :; done"]
