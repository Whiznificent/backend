# \AuthenticationApi

All URIs are relative to *http://localhost*

Method | HTTP request | Description
------------- | ------------- | -------------
[**get_current_wallet_v1**](AuthenticationApi.md#get_current_wallet_v1) | **GET** /api/v1/auth/me | 
[**get_current_wallet_v2**](AuthenticationApi.md#get_current_wallet_v2) | **GET** /api/v2/auth/me | 
[**request_wallet_nonce_v1**](AuthenticationApi.md#request_wallet_nonce_v1) | **POST** /api/v1/auth/nonce | 
[**request_wallet_nonce_v2**](AuthenticationApi.md#request_wallet_nonce_v2) | **POST** /api/v2/auth/nonce | 
[**verify_wallet_signature_v1**](AuthenticationApi.md#verify_wallet_signature_v1) | **POST** /api/v1/auth/verify | 
[**verify_wallet_signature_v2**](AuthenticationApi.md#verify_wallet_signature_v2) | **POST** /api/v2/auth/verify | 



## get_current_wallet_v1

> std::collections::HashMap<String, serde_json::Value> get_current_wallet_v1()


### Parameters

This endpoint does not need any parameter.

### Return type

[**std::collections::HashMap<String, serde_json::Value>**](serde_json::Value.md)

### Authorization

[bearerAuth](../README.md#bearerAuth)

### HTTP request headers

- **Content-Type**: Not defined
- **Accept**: application/json

[[Back to top]](#) [[Back to API list]](../README.md#documentation-for-api-endpoints) [[Back to Model list]](../README.md#documentation-for-models) [[Back to README]](../README.md)


## get_current_wallet_v2

> std::collections::HashMap<String, serde_json::Value> get_current_wallet_v2()


### Parameters

This endpoint does not need any parameter.

### Return type

[**std::collections::HashMap<String, serde_json::Value>**](serde_json::Value.md)

### Authorization

[bearerAuth](../README.md#bearerAuth)

### HTTP request headers

- **Content-Type**: Not defined
- **Accept**: application/json

[[Back to top]](#) [[Back to API list]](../README.md#documentation-for-api-endpoints) [[Back to Model list]](../README.md#documentation-for-models) [[Back to README]](../README.md)


## request_wallet_nonce_v1

> models::NonceResponse request_wallet_nonce_v1(nonce_request)


### Parameters


Name | Type | Description  | Required | Notes
------------- | ------------- | ------------- | ------------- | -------------
**nonce_request** | [**NonceRequest**](NonceRequest.md) |  | [required] |

### Return type

[**models::NonceResponse**](NonceResponse.md)

### Authorization

No authorization required

### HTTP request headers

- **Content-Type**: application/json
- **Accept**: application/json

[[Back to top]](#) [[Back to API list]](../README.md#documentation-for-api-endpoints) [[Back to Model list]](../README.md#documentation-for-models) [[Back to README]](../README.md)


## request_wallet_nonce_v2

> models::NonceResponse request_wallet_nonce_v2(nonce_request)


### Parameters


Name | Type | Description  | Required | Notes
------------- | ------------- | ------------- | ------------- | -------------
**nonce_request** | [**NonceRequest**](NonceRequest.md) |  | [required] |

### Return type

[**models::NonceResponse**](NonceResponse.md)

### Authorization

No authorization required

### HTTP request headers

- **Content-Type**: application/json
- **Accept**: application/json

[[Back to top]](#) [[Back to API list]](../README.md#documentation-for-api-endpoints) [[Back to Model list]](../README.md#documentation-for-models) [[Back to README]](../README.md)


## verify_wallet_signature_v1

> models::VerifyResponse verify_wallet_signature_v1(verify_request)


### Parameters


Name | Type | Description  | Required | Notes
------------- | ------------- | ------------- | ------------- | -------------
**verify_request** | [**VerifyRequest**](VerifyRequest.md) |  | [required] |

### Return type

[**models::VerifyResponse**](VerifyResponse.md)

### Authorization

No authorization required

### HTTP request headers

- **Content-Type**: application/json
- **Accept**: application/json

[[Back to top]](#) [[Back to API list]](../README.md#documentation-for-api-endpoints) [[Back to Model list]](../README.md#documentation-for-models) [[Back to README]](../README.md)


## verify_wallet_signature_v2

> models::VerifyResponse verify_wallet_signature_v2(verify_request)


### Parameters


Name | Type | Description  | Required | Notes
------------- | ------------- | ------------- | ------------- | -------------
**verify_request** | [**VerifyRequest**](VerifyRequest.md) |  | [required] |

### Return type

[**models::VerifyResponse**](VerifyResponse.md)

### Authorization

No authorization required

### HTTP request headers

- **Content-Type**: application/json
- **Accept**: application/json

[[Back to top]](#) [[Back to API list]](../README.md#documentation-for-api-endpoints) [[Back to Model list]](../README.md#documentation-for-models) [[Back to README]](../README.md)

