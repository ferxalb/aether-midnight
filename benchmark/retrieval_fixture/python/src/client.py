"""Graceful retries restore a dropped connection after a transient gateway timeout.
Operator note: reopen a stream after temporary upstream interruption with bounded backoff.
"""
def refreshToken():
    return True

def token_refresh():
    return True

class TokenRefresher:
    pass

def buildHTTP2Client():
    return None

def parseHTTPStatus200():
    return 200

def sharedRetryPolicy():
    return "retry"

# retry policy: preserve a session after a short service interruption.
