package dev.tslib;

/**
 * A TeamSpeak channel (immutable snapshot).
 */
public class Channel {
    public final long id;
    public final long parentId;
    public final String name;
    public final String topic;
    public final String description;
    public final int order;
    public final boolean isPermanent;
    public final boolean isSemiPermanent;
    public final boolean isDefault;
    public final boolean hasPassword;
    public final byte codec;
    public final byte codecQuality;
    public final int maxClients;
    public final int maxFamilyClients;
    public final int neededTalkPower;
    public final long iconId;

    public Channel(long id, long parentId, String name, String topic,
                   String description, int order, boolean isPermanent,
                   boolean isSemiPermanent, boolean isDefault,
                   boolean hasPassword, byte codec, byte codecQuality,
                   int maxClients, int maxFamilyClients,
                   int neededTalkPower, long iconId) {
        this.id = id;
        this.parentId = parentId;
        this.name = name;
        this.topic = topic;
        this.description = description;
        this.order = order;
        this.isPermanent = isPermanent;
        this.isSemiPermanent = isSemiPermanent;
        this.isDefault = isDefault;
        this.hasPassword = hasPassword;
        this.codec = codec;
        this.codecQuality = codecQuality;
        this.maxClients = maxClients;
        this.maxFamilyClients = maxFamilyClients;
        this.neededTalkPower = neededTalkPower;
        this.iconId = iconId;
    }

    @Override
    public String toString() {
        return "Channel(id=" + id + ", name='" + name + "')";
    }
}
