#!/usr/bin/with-contenv bashio
url="$(bashio::config 'url')"
token="$(bashio::config 'token')"
sed -i "s|@URL@|${url}|; s|@TOKEN@|${token}|" /etc/nginx/nginx.conf
exec nginx -g 'daemon off;'
