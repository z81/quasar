# AirPlay 2 on the station: shairport-sync (https://github.com/mikebrady/shairport-sync)
# writes the sound it receives to the named pipe /airplay/audio, which the bridge of
# docker/spotify reads and streams to the station. Built from the repository root:
# docker build -f docker/airplay/Dockerfile -t airplay .
# It needs the host's network: AirPlay finds it by mDNS, and AirPlay 2 keeps time over PTP.
FROM mikebrady/shairport-sync:5.6.2
COPY docker/airplay/shairport-sync.conf /etc/shairport-sync.conf
COPY docker/airplay/airplay-volume /usr/local/bin/airplay-volume
RUN chmod 755 /usr/local/bin/airplay-volume
